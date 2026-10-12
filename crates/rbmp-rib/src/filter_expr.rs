use std::collections::HashSet;
use pest::Parser;
use pest_derive::Parser;

// ─── Pest grammar ─────────────────────────────────────────────────────────────

#[derive(Parser)]
#[grammar = "filter.pest"]
struct FilterParser;

// ─── Route context ────────────────────────────────────────────────────────────

/// Route-level context made available to filter expressions.
/// Built once per route event; fields are pre-computed from `PathAttributes`.
#[derive(Debug, Clone)]
pub struct RouteCtx {
    pub prefix_len:    u8,
    pub as_path_len:   usize,
    pub origin_asn:    u32,
    pub has_prepend:   bool,
    pub rpki:          String,  // "valid" | "invalid" | "not-found" | "unknown"
    pub action:        String,  // "announce" | "withdraw"
    pub peer_as:       u32,
    pub local_pref:    Option<u32>,
    pub med:           Option<u32>,
    pub community_set: HashSet<String>,
}

// ─── Expression AST ───────────────────────────────────────────────────────────

/// Compiled filter expression evaluated per route event.
#[derive(Debug, Clone)]
pub enum Expr {
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),

    // numeric comparisons
    PrefixLenGt(u8),
    PrefixLenLt(u8),
    PrefixLenGe(u8),
    PrefixLenLe(u8),
    PrefixLenEq(u8),
    AsPathLenGt(usize),
    AsPathLenLt(usize),
    AsPathLenGe(usize),
    AsPathLenLe(usize),
    AsPathLenEq(usize),
    LocalPrefGt(u32),
    LocalPrefLt(u32),
    LocalPrefEq(u32),
    MedGt(u32),
    MedLt(u32),
    MedEq(u32),

    // string / set comparisons
    RpkiEq(String),
    RpkiNe(String),
    ActionEq(String),
    CommunityHas(String),

    // membership
    OriginAsIn(HashSet<u32>),
    OriginAsNotIn(HashSet<u32>),
    PeerAsIn(HashSet<u32>),
    PeerAsNotIn(HashSet<u32>),

    // flags
    HasPrepend,
    True,
    False,
}

impl Expr {
    /// Evaluate the expression against a route context.
    pub fn eval(&self, ctx: &RouteCtx) -> bool {
        match self {
            Self::And(a, b)           => a.eval(ctx) && b.eval(ctx),
            Self::Or(a, b)            => a.eval(ctx) || b.eval(ctx),
            Self::Not(e)              => !e.eval(ctx),

            Self::PrefixLenGt(n)      => ctx.prefix_len > *n,
            Self::PrefixLenLt(n)      => ctx.prefix_len < *n,
            Self::PrefixLenGe(n)      => ctx.prefix_len >= *n,
            Self::PrefixLenLe(n)      => ctx.prefix_len <= *n,
            Self::PrefixLenEq(n)      => ctx.prefix_len == *n,
            Self::AsPathLenGt(n)      => ctx.as_path_len > *n,
            Self::AsPathLenLt(n)      => ctx.as_path_len < *n,
            Self::AsPathLenGe(n)      => ctx.as_path_len >= *n,
            Self::AsPathLenLe(n)      => ctx.as_path_len <= *n,
            Self::AsPathLenEq(n)      => ctx.as_path_len == *n,
            Self::LocalPrefGt(n)      => ctx.local_pref.unwrap_or(100) > *n,
            Self::LocalPrefLt(n)      => ctx.local_pref.unwrap_or(100) < *n,
            Self::LocalPrefEq(n)      => ctx.local_pref.unwrap_or(100) == *n,
            Self::MedGt(n)            => ctx.med.unwrap_or(0) > *n,
            Self::MedLt(n)            => ctx.med.unwrap_or(0) < *n,
            Self::MedEq(n)            => ctx.med.unwrap_or(0) == *n,

            Self::RpkiEq(v)           => ctx.rpki == *v,
            Self::RpkiNe(v)           => ctx.rpki != *v,
            Self::ActionEq(v)         => ctx.action == *v,
            Self::CommunityHas(c)     => ctx.community_set.contains(c),

            Self::OriginAsIn(s)       => s.contains(&ctx.origin_asn),
            Self::OriginAsNotIn(s)    => !s.contains(&ctx.origin_asn),
            Self::PeerAsIn(s)         => s.contains(&ctx.peer_as),
            Self::PeerAsNotIn(s)      => !s.contains(&ctx.peer_as),

            Self::HasPrepend          => ctx.has_prepend,
            Self::True                => true,
            Self::False               => false,
        }
    }
}

// ─── Parser ───────────────────────────────────────────────────────────────────

/// Parse a filter expression string into a compiled `Expr` AST.
///
/// # Errors
/// Returns a `Box<dyn Error>` if the expression fails to parse or contains
/// an unknown field/operator combination.
pub fn parse_expr(input: &str) -> Result<Expr, Box<dyn std::error::Error + Send + Sync>> {
    let pairs = FilterParser::parse(Rule::expr, input.trim())
        .map_err(|e| format!("filter expression parse error: {e}"))?;
    let pair = pairs.into_iter().next().ok_or("empty expression")?;
    build_expr(pair)
}

fn build_expr(pair: pest::iterators::Pair<Rule>) -> Result<Expr, Box<dyn std::error::Error + Send + Sync>> {
    match pair.as_rule() {
        Rule::expr => {
            // expr = { or_expr ~ EOI } — unwrap to the or_expr child
            let inner = pair.into_inner()
                .find(|p| p.as_rule() == Rule::or_expr)
                .ok_or("expr missing or_expr child")?;
            build_expr(inner)
        }
        Rule::or_expr => {
            let mut inner = pair.into_inner();
            let mut left = build_expr(inner.next().ok_or("missing lhs")?)?;
            while let Some(rhs) = inner.next() {
                left = Expr::Or(Box::new(left), Box::new(build_expr(rhs)?));
            }
            Ok(left)
        }
        Rule::and_expr => {
            let mut inner = pair.into_inner();
            let mut left = build_expr(inner.next().ok_or("missing lhs")?)?;
            while let Some(rhs) = inner.next() {
                left = Expr::And(Box::new(left), Box::new(build_expr(rhs)?));
            }
            Ok(left)
        }
        Rule::not_expr => {
            let mut inner = pair.into_inner();
            let first = inner.next().ok_or("missing operand")?;
            if first.as_rule() == Rule::not_kw {
                // NOT <atom>
                let atom = inner.next().ok_or("missing atom after NOT")?;
                Ok(Expr::Not(Box::new(build_expr(atom)?)))
            } else {
                // plain <atom>
                build_expr(first)
            }
        }
        Rule::atom => {
            let inner = pair.into_inner().next().ok_or("empty atom")?;
            build_expr(inner)
        }
        Rule::comparison => {
            let mut inner = pair.into_inner();
            let field = inner.next().ok_or("missing field")?.as_str().trim().to_string();
            let op    = inner.next().ok_or("missing op")?.as_str().trim().to_string();
            let val   = inner.next().ok_or("missing value")?;
            build_comparison(&field, &op, val)
        }
        Rule::membership => {
            let mut inner = pair.into_inner();
            let field  = inner.next().ok_or("missing field")?.as_str().trim().to_string();
            // Grammar: field ~ not_kw? ~ in_kw ~ "[" ~ int_list ~ "]"
            // in_kw is a silent rule (_) so it never appears in .into_inner().
            // After field: either not_kw (negated) or int_list (non-negated).
            let next = inner.next().ok_or("missing NOT/int_list")?;
            let (negated, int_list_pair) = if next.as_rule() == Rule::not_kw {
                (true, inner.next().ok_or("missing int_list after NOT IN")?)
            } else {
                // next IS the int_list
                (false, next)
            };
            let set: HashSet<u32> = int_list_pair
                .into_inner()
                .filter(|p| p.as_rule() == Rule::integer)
                .map(|p| p.as_str().trim().parse::<u32>())
                .collect::<Result<_, _>>()?;
            match field.as_str() {
                "origin_as" if !negated  => Ok(Expr::OriginAsIn(set)),
                "origin_as" if negated   => Ok(Expr::OriginAsNotIn(set)),
                "peer_as"   if !negated  => Ok(Expr::PeerAsIn(set)),
                "peer_as"   if negated   => Ok(Expr::PeerAsNotIn(set)),
                _ => Err(format!("IN membership not supported for field '{field}'").into()),
            }
        }
        Rule::flag => {
            match pair.as_str().trim() {
                "has_prepend" => Ok(Expr::HasPrepend),
                "true"        => Ok(Expr::True),
                "false"       => Ok(Expr::False),
                other => Err(format!("unknown flag: {other}").into()),
            }
        }
        r => Err(format!("unexpected rule: {r:?}").into()),
    }
}

fn build_comparison(
    field: &str,
    op:    &str,
    val:   pest::iterators::Pair<Rule>,
) -> Result<Expr, Box<dyn std::error::Error + Send + Sync>> {
    let val_inner = val.into_inner().next().ok_or("empty value")?;
    match (field, op) {
        ("prefix_len", op) => {
            let n: u8 = val_inner.as_str().trim().parse()?;
            match op { ">" => Ok(Expr::PrefixLenGt(n)), "<" => Ok(Expr::PrefixLenLt(n)),
                ">=" => Ok(Expr::PrefixLenGe(n)), "<=" => Ok(Expr::PrefixLenLe(n)),
                "==" | "=" => Ok(Expr::PrefixLenEq(n)),
                _ => Err(format!("unsupported op '{op}' for prefix_len").into()) }
        }
        ("as_path_len", op) => {
            let n: usize = val_inner.as_str().trim().parse()?;
            match op { ">" => Ok(Expr::AsPathLenGt(n)), "<" => Ok(Expr::AsPathLenLt(n)),
                ">=" => Ok(Expr::AsPathLenGe(n)), "<=" => Ok(Expr::AsPathLenLe(n)),
                "==" | "=" => Ok(Expr::AsPathLenEq(n)),
                _ => Err(format!("unsupported op '{op}' for as_path_len").into()) }
        }
        ("local_pref", op) => {
            let n: u32 = val_inner.as_str().trim().parse()?;
            match op { ">" => Ok(Expr::LocalPrefGt(n)), "<" => Ok(Expr::LocalPrefLt(n)),
                "==" | "=" => Ok(Expr::LocalPrefEq(n)),
                _ => Err(format!("unsupported op '{op}' for local_pref").into()) }
        }
        ("med", op) => {
            let n: u32 = val_inner.as_str().trim().parse()?;
            match op { ">" => Ok(Expr::MedGt(n)), "<" => Ok(Expr::MedLt(n)),
                "==" | "=" => Ok(Expr::MedEq(n)),
                _ => Err(format!("unsupported op '{op}' for med").into()) }
        }
        ("rpki", "==") | ("rpki", "=") => {
            let s = strip_quotes(val_inner.as_str());
            Ok(Expr::RpkiEq(s))
        }
        ("rpki", "!=") => {
            let s = strip_quotes(val_inner.as_str());
            Ok(Expr::RpkiNe(s))
        }
        ("action", "==") | ("action", "=") => {
            let s = strip_quotes(val_inner.as_str());
            Ok(Expr::ActionEq(s))
        }
        ("community", "==") | ("community", "=") => {
            let s = strip_quotes(val_inner.as_str());
            Ok(Expr::CommunityHas(s))
        }
        (f, o) => Err(format!("unsupported field/op combination: '{f}' '{o}'").into()),
    }
}

fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('\'') && s.ends_with('\'')) || (s.starts_with('"') && s.ends_with('"')) {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> RouteCtx {
        let mut community_set = HashSet::new();
        community_set.insert("65000:100".to_string());
        RouteCtx {
            prefix_len:    24,
            as_path_len:   4,
            origin_asn:    64496,
            has_prepend:   false,
            rpki:          "invalid".to_string(),
            action:        "announce".to_string(),
            peer_as:       65001,
            local_pref:    Some(150),
            med:           Some(50),
            community_set,
        }
    }

    #[test]
    fn test_simple_comparison() {
        let expr = parse_expr("prefix_len > 22").unwrap();
        assert!(expr.eval(&ctx()));
        let expr2 = parse_expr("prefix_len > 24").unwrap();
        assert!(!expr2.eval(&ctx()));
    }

    #[test]
    fn test_rpki_eq() {
        let expr = parse_expr("rpki == 'invalid'").unwrap();
        assert!(expr.eval(&ctx()));
        let expr2 = parse_expr("rpki == 'valid'").unwrap();
        assert!(!expr2.eval(&ctx()));
    }

    #[test]
    fn test_and_compound() {
        let expr = parse_expr("rpki == 'invalid' AND prefix_len > 22").unwrap();
        assert!(expr.eval(&ctx()));
    }

    #[test]
    fn test_not() {
        let expr = parse_expr("NOT rpki == 'valid'").unwrap();
        assert!(expr.eval(&ctx()));
    }

    #[test]
    fn test_or() {
        let expr = parse_expr("rpki == 'valid' OR prefix_len > 22").unwrap();
        assert!(expr.eval(&ctx()));
    }

    #[test]
    fn test_in_set() {
        let expr = parse_expr("peer_as IN [65001, 65002, 65003]").unwrap();
        assert!(expr.eval(&ctx()));
        let expr2 = parse_expr("origin_as IN [64496, 64497]").unwrap();
        assert!(expr2.eval(&ctx()));
    }

    #[test]
    fn test_community_has() {
        let expr = parse_expr("community == '65000:100'").unwrap();
        assert!(expr.eval(&ctx()));
    }

    #[test]
    fn test_has_prepend() {
        let expr = parse_expr("has_prepend").unwrap();
        assert!(!expr.eval(&ctx()));

        let mut ctx2 = ctx();
        ctx2.has_prepend = true;
        assert!(expr.eval(&ctx2));
    }

    #[test]
    fn test_complex_expr() {
        // reject rpki invalid AND too specific
        let expr = parse_expr("rpki == 'invalid' AND prefix_len > 23").unwrap();
        assert!(expr.eval(&ctx()));
    }

    #[test]
    fn test_action_eq() {
        let expr = parse_expr("action == 'announce'").unwrap();
        assert!(expr.eval(&ctx()));
    }

    #[test]
    fn test_local_pref() {
        let expr = parse_expr("local_pref > 100").unwrap();
        assert!(expr.eval(&ctx()));
    }

    #[test]
    fn test_med_lt() {
        let expr = parse_expr("med < 100").unwrap();
        assert!(expr.eval(&ctx()), "med=50 < 100 must be true");
    }

    #[test]
    fn test_med_gt_false() {
        let expr = parse_expr("med > 100").unwrap();
        assert!(!expr.eval(&ctx()), "med=50 > 100 must be false");
    }

    #[test]
    fn test_local_pref_lt_false() {
        let expr = parse_expr("local_pref < 100").unwrap();
        assert!(!expr.eval(&ctx()), "local_pref=150 < 100 must be false");
    }

    #[test]
    fn test_local_pref_eq() {
        let expr = parse_expr("local_pref == 150").unwrap();
        assert!(expr.eval(&ctx()), "local_pref=150 == 150 must be true");
    }

    #[test]
    fn test_as_path_len_gt() {
        let expr = parse_expr("as_path_len > 3").unwrap();
        assert!(expr.eval(&ctx()), "as_path_len=4 > 3 must be true");
    }

    #[test]
    fn test_as_path_len_le() {
        let expr = parse_expr("as_path_len <= 4").unwrap();
        assert!(expr.eval(&ctx()), "as_path_len=4 <= 4 must be true");
    }

    #[test]
    fn test_origin_as_not_in() {
        let expr = parse_expr("origin_as NOT IN [1, 2, 3]").unwrap();
        assert!(expr.eval(&ctx()), "origin_asn=64496 NOT IN [1,2,3] must be true");
    }

    #[test]
    fn test_peer_as_not_in() {
        let expr = parse_expr("peer_as NOT IN [65000, 65002]").unwrap();
        assert!(expr.eval(&ctx()), "peer_as=65001 NOT IN [65000,65002] must be true");
    }

    #[test]
    fn test_community_missing() {
        let expr = parse_expr("community == '65000:200'").unwrap();
        assert!(!expr.eval(&ctx()), "community 65000:200 is absent in ctx");
    }

    #[test]
    fn test_action_withdraw() {
        let expr = parse_expr("action == 'withdraw'").unwrap();
        assert!(!expr.eval(&ctx()), "action=announce != withdraw");

        let mut ctx2 = ctx();
        ctx2.action = "withdraw".to_string();
        assert!(expr.eval(&ctx2));
    }

    #[test]
    fn test_nested_and_or() {
        // (prefix_len > 20 AND rpki == 'invalid') OR peer_as IN [65002]
        let expr = parse_expr("(prefix_len > 20 AND rpki == 'invalid') OR peer_as IN [65002]").unwrap();
        assert!(expr.eval(&ctx()), "prefix_len=24>20 AND rpki=invalid → true, so whole OR is true");
    }

    #[test]
    fn test_parse_error_bad_syntax() {
        let result = parse_expr("AND AND AND");
        assert!(result.is_err(), "invalid syntax must return Err");
    }

    #[test]
    fn test_prefix_len_ge_boundary() {
        let expr = parse_expr("prefix_len >= 24").unwrap();
        assert!(expr.eval(&ctx()), "prefix_len=24 >= 24 boundary must be true");

        let expr2 = parse_expr("prefix_len >= 25").unwrap();
        assert!(!expr2.eval(&ctx()), "prefix_len=24 >= 25 must be false");
    }
}
