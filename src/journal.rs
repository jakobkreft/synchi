use std::fmt;
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone)]
pub enum OpResult {
    Success,
    Failed(String),
}

#[derive(Debug, Default, Clone)]
pub struct ExecutionStats {
    pub copies_a_to_b: usize,
    pub bytes_a_to_b: u64,
    pub copies_b_to_a: usize,
    pub bytes_b_to_a: u64,
    pub deletes_on_a: usize,
    pub deletes_on_b: usize,
}

#[derive(Debug, Clone)]
pub struct JournalEntry {
    pub op: Operation,
    pub result: OpResult,
}

#[derive(Debug)]
pub struct Journal {
    pub entries: Vec<JournalEntry>,
    start_time: SystemTime,
    stats: Option<ExecutionStats>,
    overall_duration: Option<Duration>,
}

impl Journal {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            start_time: SystemTime::now(),
            stats: None,
            overall_duration: None,
        }
    }
    pub fn record(&mut self, op: Operation, result: OpResult) {
        self.entries.push(JournalEntry { op, result });
    }

    pub fn set_stats(&mut self, stats: ExecutionStats) {
        self.stats = Some(stats);
    }

    pub fn set_overall_duration(&mut self, duration: Duration) {
        self.overall_duration = Some(duration);
    }

    pub fn summary(&self) -> String {
        let total = self.entries.len();
        let success = self
            .entries
            .iter()
            .filter(|e| matches!(e.result, OpResult::Success))
            .count();
        let failed = self
            .entries
            .iter()
            .filter(|e| matches!(e.result, OpResult::Failed(_)))
            .count();

        let elapsed = self.start_time.elapsed().unwrap_or_default();

        let mut s = String::new();
        s.push_str("=== Sync Report ===\n");
        s.push_str(&format!("Total Operations: {}\n", total));
        s.push_str(&format!("Success: {}\n", success));
        s.push_str(&format!("Failed:  {}\n", failed));
        s.push_str(&format!("Transfer Time: {:.2?}\n", elapsed));

        if let Some(stats) = &self.stats {
            s.push_str("\nTransfer Summary:\n");
            s.push_str(&format!(
                "  Copy A → B: {} files ({})\n",
                stats.copies_a_to_b,
                format_bytes(stats.bytes_a_to_b)
            ));
            s.push_str(&format!(
                "  Copy B → A: {} files ({})\n",
                stats.copies_b_to_a,
                format_bytes(stats.bytes_b_to_a)
            ));
            s.push_str(&format!(
                "  Total Copied: {}\n",
                format_bytes(stats.bytes_a_to_b + stats.bytes_b_to_a)
            ));
            s.push_str(&format!("  Deleted on A: {} entries\n", stats.deletes_on_a));
            s.push_str(&format!("  Deleted on B: {} entries\n", stats.deletes_on_b));
        }

        if let Some(overall) = self.overall_duration {
            s.push_str(&format!("\nOverall Duration: {:.2?}\n", overall));
        }

        if failed > 0 {
            s.push_str("\n--- Failures ---\n");
            for e in self
                .entries
                .iter()
                .filter(|e| matches!(e.result, OpResult::Failed(_)))
            {
                if let OpResult::Failed(ref msg) = e.result {
                    s.push_str(&format!(
                        "{} [{}] {}: {}\n",
                        e.op.path, e.op.action, e.op.detail, msg
                    ));
                }
            }
        }

        s
    }
}

impl Default for Journal {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Journal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.summary())
    }
}

#[derive(Debug, Clone)]
pub struct Operation {
    pub path: String,
    pub action: String,
    pub detail: String,
}

impl Operation {
    pub fn new(
        path: impl Into<String>,
        action: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            action: action.into(),
            detail: detail.into(),
        }
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{:.2} {}", value, UNITS[unit])
    }
}
