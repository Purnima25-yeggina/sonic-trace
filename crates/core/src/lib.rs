//! Pipeline model and report types. No I/O, no SONiC knowledge.
use serde::Serialize;

/// A stage an object passes through on its way to hardware.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Stage {
    pub name: String,
    pub owner: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Presence {
    Present,
    Missing,
}

#[derive(Debug, Clone, Serialize)]
pub struct StageResult {
    pub stage: Stage,
    pub presence: Presence,
    /// Short human-readable detail, e.g. the key found.
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnosis {
    /// `None` when every stage is present or evidence is insufficient.
    pub stopped_at: Option<String>,
    pub message: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub object: String,
    pub stages: Vec<StageResult>,
    pub diagnosis: Diagnosis,
}

impl Report {
    pub fn render_text(&self) -> String {
        let mut out = format!("trace: {}\n", self.object);
        for s in &self.stages {
            let mark = match s.presence {
                Presence::Present => "present",
                Presence::Missing => "MISSING",
            };
            out.push_str(&format!(
                "  {:<10} {:<8} {}\n",
                s.stage.name, mark, s.detail
            ));
        }
        out.push_str(&format!("=> {}\n", self.diagnosis.message));
        for e in &self.diagnosis.evidence {
            out.push_str(&format!("   evidence: {e}\n"));
        }
        out
    }
}
