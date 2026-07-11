use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::process::{Command, Stdio};

use super::config::Block;

const CMDLENGTH: usize = 50;

pub type StatusCache = Vec<String>;

#[inline]
pub fn fast_hash(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

pub fn getcmd(block: &Block) -> String {
    let suffix = Command::new("sh")
        .arg("-c")
        .arg(&block.command)
        .stdout(Stdio::piped())
        .output()
        .ok()
        .filter(|r| r.status.success())
        .map(|r| {
            let raw = String::from_utf8_lossy(&r.stdout);
            let max_len = CMDLENGTH
                .saturating_sub(block.icon.len())
                .saturating_sub(2); // rough delim allowance
            let trimmed = raw.trim_end_matches('\n');
            trimmed[..trimmed.len().min(max_len)].to_owned()
        })
        .unwrap_or_default();
    format!("{}{}", block.icon, suffix)
}

pub fn initial_cache(blocks: &[Block]) -> StatusCache {
    blocks.iter().map(getcmd).collect()
}

pub fn update_cache(time: u32, signal_mask: u32, prev_cache: StatusCache, blocks: &[Block]) -> StatusCache {
    blocks
        .iter()
        .zip(prev_cache)
        .map(|(block, previous)| {
            let by_time = time == 0 || time == u32::MAX || (block.interval != 0 && time.is_multiple_of(block.interval));
            let by_signal = block.signal != 0 && (signal_mask >> u32::from(block.signal)) & 1 == 1;
            if by_time || by_signal { getcmd(block) } else { previous }
        })
        .collect()
}

pub fn build_status(cache: &[String], delim: &str) -> String {
    if delim.is_empty() {
        return cache.concat();
    }
    cache.join(delim)
}
