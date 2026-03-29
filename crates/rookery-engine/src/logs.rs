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

    #[tokio::test]
    async fn test_subscribe_receives_pushed_messages() {
        let buf = LogBuffer::new(100);
        let mut rx = buf.subscribe();

        buf.push("hello".into());
        buf.push("world".into());

        let msg1 = rx.recv().await.unwrap();
        let msg2 = rx.recv().await.unwrap();
        assert_eq!(msg1, "hello");
        assert_eq!(msg2, "world");
    }

    #[test]
    fn test_len_and_is_empty_after_push() {
        let buf = LogBuffer::new(100);
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);

        buf.push("first".into());
        assert!(!buf.is_empty());
        assert_eq!(buf.len(), 1);

        buf.push("second".into());
        assert_eq!(buf.len(), 2);

        // Push beyond capacity to verify len stays at capacity
        let small_buf = LogBuffer::new(2);
        small_buf.push("a".into());
        small_buf.push("b".into());
        small_buf.push("c".into()); // evicts "a"
        assert_eq!(small_buf.len(), 2);
    }

    #[tokio::test]
    async fn test_concurrent_push_from_multiple_tasks() {
        use std::sync::Arc;

        let buf = Arc::new(LogBuffer::new(1000));
        let mut handles = Vec::new();

        for task_id in 0..10 {
            let buf_clone = Arc::clone(&buf);
            handles.push(tokio::spawn(async move {
                for i in 0..100 {
                    buf_clone.push(format!("task{task_id}-line{i}"));
                }
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        // All 1000 lines pushed, buffer capacity is 1000
        assert_eq!(buf.len(), 1000);
    }
}
