/// D4-14 T3 — Vault re-key utility.
///
/// Usage:
///   BONSAI_VAULT_PASSPHRASE=<old> BONSAI_VAULT_NEW_PASSPHRASE=<new> \
///       ./vault-rekey [vault-root]
///
/// vault-root defaults to the `runtime_dir` value used by bonsai (typically
/// the directory that contains `vault.age`).  Pass the absolute path when
/// running from a different directory.
///
/// After success, restart bonsai with `BONSAI_VAULT_PASSPHRASE=<new>`.
fn main() {
    let vault_root = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".".to_string());

    println!("bonsai vault-rekey: vault root = {vault_root}");

    let vault = bonsai::credentials::CredentialVault::open(
        &vault_root,
        "BONSAI_VAULT_PASSPHRASE",
    )
    .unwrap_or_else(|e| {
        eprintln!("ERROR: failed to open vault: {e:#}");
        std::process::exit(1);
    });

    vault
        .rekey("BONSAI_VAULT_NEW_PASSPHRASE")
        .unwrap_or_else(|e| {
            eprintln!("ERROR: rekey failed: {e:#}");
            std::process::exit(2);
        });

    println!("vault-rekey: SUCCESS — vault re-encrypted with new passphrase.");
    println!("Restart bonsai with BONSAI_VAULT_PASSPHRASE set to the new value.");
}
