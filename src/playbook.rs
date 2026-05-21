use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Deserialize, Clone, Debug, serde::Serialize)]
pub struct PlaybookEntry {
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub operation: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub risk_tier: String,
    #[serde(default)]
    pub steps: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct PlaybookFile {
    pub detection_rule_id: String,
    #[serde(default)]
    pub playbooks: Vec<PlaybookEntry>,
}

pub struct PlaybookLibrary {
    entries: HashMap<String, Vec<PlaybookEntry>>,
}

impl PlaybookLibrary {
    pub fn load_dir(dir: &str) -> Arc<Self> {
        let mut entries: HashMap<String, Vec<PlaybookEntry>> = HashMap::new();
        let Ok(read_dir) = std::fs::read_dir(dir) else {
            tracing::warn!(dir, "playbook library dir not found or unreadable");
            return Arc::new(Self { entries });
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "yaml" && ext != "yml" {
                continue;
            }
            let Ok(raw) = std::fs::read_to_string(&path) else { continue };
            match serde_yaml::from_str::<PlaybookFile>(&raw) {
                Ok(pf) if !pf.playbooks.is_empty() => {
                    entries
                        .entry(pf.detection_rule_id)
                        .or_default()
                        .extend(pf.playbooks);
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "failed to parse playbook file");
                }
            }
        }
        tracing::info!(rules = entries.len(), "playbook library loaded");
        Arc::new(Self { entries })
    }

    /// Return all (rule_id, entries) pairs for the catalog endpoint.
    pub fn catalog(&self) -> &std::collections::HashMap<String, Vec<PlaybookEntry>> {
        &self.entries
    }

    /// Return a matching playbook for `rule_id`. Prefers the first vendor-specific match
    /// when `vendor` is Some; otherwise returns the first entry for the rule.
    pub fn find(&self, rule_id: &str, vendor: Option<&str>) -> Option<&PlaybookEntry> {
        let list = self.entries.get(rule_id)?;
        if let Some(v) = vendor {
            let v_lc = v.to_ascii_lowercase();
            if let Some(pb) = list
                .iter()
                .find(|pb| pb.vendor.to_ascii_lowercase().contains(&v_lc))
            {
                return Some(pb);
            }
        }
        list.first()
    }
}
