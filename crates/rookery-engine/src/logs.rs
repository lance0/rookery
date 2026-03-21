use std::collections::VecDeque;
use std::sync::RwLock;
use tokio::sync::broadcast;

pub struct LogBuffer {
    lines: RwLock<VecDeque<String>>,
    capacity: usize,
    tx: broadcast::Sender<String>,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            lines: RwLock::new(VecDeque::with_capacity(capacity)),
            capacity,
            tx,
        }
    }

    pub fn push(&self, line: String) {
        let _ = self.tx.send(line.clone());

        let mut lines = self.lines.write().unwrap_or_else(|e| e.into_inner());
        if lines.len() >= self.capacity {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    pub fn last_n(&self, n: usize) -> Vec<String> {
        let lines = self.lines.read().unwrap_or_else(|e| e.into_inner());
        lines.iter().rev().take(n).rev().cloned().collect()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }

    pub fn len(&self) -> usize {
        self.lines.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ring_buffer() {
        let buf = LogBuffer::new(3);
        buf.push("a".into());
        buf.push("b".into());
        buf.push("c".into());
        buf.push("d".into()); // "a" should be evicted

        let lines = buf.last_n(10);
        assert_eq!(lines, vec!["b", "c", "d"]);
    }

    #[test]
    fn test_last_n() {
        let buf = LogBuffer::new(100);
        for i in 0..10 {
            buf.push(format!("line {i}"));
        }

        let last3 = buf.last_n(3);
        assert_eq!(last3, vec!["line 7", "line 8", "line 9"]);
    }
}
