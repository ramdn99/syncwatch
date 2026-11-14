//! Turns the policy's cursors into stream operations (ADR 0009). The diff is pure; the
//! [`Juggler`] applies it through a small [`StreamDriver`] trait, so it is tested with a fake
//! driver and runs on real librqbit `FileStream`s in the engine.

use crate::policy::{Cursor, CursorKind};
use futures_util::future::BoxFuture;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Open { kind: CursorKind, pos: u64 },
    Seek { kind: CursorKind, pos: u64 },
    Close { kind: CursorKind },
}

/// Minimal operations to go from `current` to `desired` (one stream per kind). Closes come
/// first, so dropping auxiliary streams (exclusive mode) takes effect before anything opens.
pub fn diff(current: &BTreeMap<CursorKind, u64>, desired: &[Cursor]) -> Vec<Op> {
    let mut ops = Vec::new();
    for k in current.keys() {
        if desired.iter().all(|c| c.kind != *k) {
            ops.push(Op::Close { kind: *k });
        }
    }
    for c in desired {
        match current.get(&c.kind) {
            None => ops.push(Op::Open {
                kind: c.kind,
                pos: c.pos,
            }),
            Some(p) if *p != c.pos => ops.push(Op::Seek {
                kind: c.kind,
                pos: c.pos,
            }),
            Some(_) => {}
        }
    }
    ops
}

/// What the juggler needs from the engine.
pub trait StreamDriver: Send + Sync {
    type Stream: Send;
    fn open(&self, pos: u64) -> BoxFuture<'_, Option<Self::Stream>>;
    fn seek<'a>(&'a self, s: &'a mut Self::Stream, pos: u64) -> BoxFuture<'a, bool>;
}

pub struct Juggler<S> {
    open: BTreeMap<CursorKind, (S, u64)>,
}

impl<S: Send> Default for Juggler<S> {
    fn default() -> Self {
        Self {
            open: BTreeMap::new(),
        }
    }
}

impl<S: Send> Juggler<S> {
    pub fn positions(&self) -> BTreeMap<CursorKind, u64> {
        self.open.iter().map(|(k, (_, p))| (*k, *p)).collect()
    }

    /// Applies the plan's cursors. Returns the operations performed.
    pub async fn apply<D: StreamDriver<Stream = S>>(
        &mut self,
        driver: &D,
        desired: &[Cursor],
    ) -> Vec<Op> {
        let ops = diff(&self.positions(), desired);
        for op in &ops {
            match *op {
                Op::Close { kind } => {
                    // Dropping the FileStream unregisters it from librqbit's priority list.
                    self.open.remove(&kind);
                }
                Op::Open { kind, pos } => {
                    if let Some(s) = driver.open(pos).await {
                        self.open.insert(kind, (s, pos));
                    }
                }
                Op::Seek { kind, pos } => {
                    let ok = match self.open.get_mut(&kind) {
                        Some((s, p)) => {
                            let ok = driver.seek(s, pos).await;
                            if ok {
                                *p = pos;
                            }
                            ok
                        }
                        None => false,
                    };
                    if !ok {
                        self.open.remove(&kind);
                    }
                }
            }
        }
        ops
    }

    /// Drops every stream (torrent stopped).
    pub fn clear(&mut self) {
        self.open.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Fake {
        log: Mutex<Vec<String>>,
    }

    impl StreamDriver for Fake {
        type Stream = u64;
        fn open(&self, pos: u64) -> BoxFuture<'_, Option<u64>> {
            if let Ok(mut l) = self.log.lock() {
                l.push(format!("open {pos}"));
            }
            Box::pin(async move { Some(pos) })
        }
        fn seek<'a>(&'a self, s: &'a mut u64, pos: u64) -> BoxFuture<'a, bool> {
            if let Ok(mut l) = self.log.lock() {
                l.push(format!("seek {s}->{pos}"));
            }
            *s = pos;
            Box::pin(async { true })
        }
    }

    fn c(kind: CursorKind, pos: u64) -> Cursor {
        Cursor { kind, pos }
    }

    #[test]
    fn diff_closes_first_then_opens_and_seeks() {
        let mut cur = BTreeMap::new();
        cur.insert(CursorKind::Lead, 0);
        cur.insert(CursorKind::Demand, 90);
        cur.insert(CursorKind::Fill, 50);
        let ops = diff(&cur, &[c(CursorKind::Lead, 40)]);
        assert_eq!(
            ops,
            vec![
                Op::Close {
                    kind: CursorKind::Demand
                },
                Op::Close {
                    kind: CursorKind::Fill
                },
                Op::Seek {
                    kind: CursorKind::Lead,
                    pos: 40
                }
            ]
        );
        assert!(
            diff(
                &BTreeMap::from([(CursorKind::Lead, 40)]),
                &[c(CursorKind::Lead, 40)]
            )
            .is_empty()
        );
    }

    #[test]
    fn juggler_follows_the_plan_through_the_driver() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap_or_else(|e| panic!("runtime: {e}"));
        rt.block_on(async {
            let d = Fake::default();
            let mut j: Juggler<u64> = Juggler::default();
            j.apply(&d, &[c(CursorKind::Lead, 0), c(CursorKind::Demand, 90)])
                .await;
            j.apply(&d, &[c(CursorKind::Lead, 40)]).await;
            j.apply(&d, &[c(CursorKind::Lead, 40), c(CursorKind::Fill, 70)])
                .await;
            assert_eq!(
                j.positions(),
                BTreeMap::from([(CursorKind::Lead, 40), (CursorKind::Fill, 70)])
            );
            let log = d.log.lock().map(|l| l.clone()).unwrap_or_default();
            assert_eq!(log, vec!["open 0", "open 90", "seek 0->40", "open 70"]);
        });
    }
}
