use crossbeam_channel::{Receiver, Sender};
use livestage_core::ShowCommand;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlSource {
    Ui,
    Keyboard,
    Osc { peer: String },
    Midi { port: String },
    WebSocket { peer: String },
    Cue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ControlEvent {
    pub source: ControlSource,
    pub command: ShowCommand,
}

/// Multi-producer, single-consumer funnel for show commands.
#[derive(Clone)]
pub struct ControlHub {
    tx: Sender<ControlEvent>,
    rx: Receiver<ControlEvent>,
}

impl Default for ControlHub {
    fn default() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self { tx, rx }
    }
}

impl ControlHub {
    pub fn sender(&self) -> Sender<ControlEvent> {
        self.tx.clone()
    }
    pub fn send(&self, source: ControlSource, command: ShowCommand) {
        let _ = self.tx.send(ControlEvent { source, command });
    }
    /// Drains all pending events (non-blocking).
    pub fn drain(&self) -> impl Iterator<Item = ControlEvent> + '_ {
        self.rx.try_iter()
    }
}
