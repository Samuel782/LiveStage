//! Global panic button logic.
//!
//! One press = *soft* panic (fast fade of audio/video, cues stopped).
//! A second press within the escalation window = *hard* panic (instant cut
//! everywhere, DMX intensity blackout). This mirrors how operators actually
//! behave under stress: hit it, then hit it again if it's not quiet *now*.

use livestage_core::ShowCommand;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct PanicController {
    last: Option<Instant>,
    pub escalation_window: Duration,
}

impl Default for PanicController {
    fn default() -> Self {
        Self { last: None, escalation_window: Duration::from_millis(1_500) }
    }
}

impl PanicController {
    pub fn press(&mut self, now: Instant) -> ShowCommand {
        let hard = self.last.is_some_and(|t| now.duration_since(t) <= self.escalation_window);
        self.last = Some(now);
        ShowCommand::Panic { hard }
    }

    /// Converts any incoming panic into the correctly escalated one.
    pub fn filter(&mut self, cmd: ShowCommand, now: Instant) -> ShowCommand {
        match cmd {
            ShowCommand::Panic { hard: true } => {
                self.last = Some(now);
                cmd
            }
            ShowCommand::Panic { hard: false } => self.press(now),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn double_press_escalates() {
        let mut p = PanicController::default();
        let t = Instant::now();
        assert_eq!(p.press(t), ShowCommand::Panic { hard: false });
        assert_eq!(p.press(t + Duration::from_millis(400)), ShowCommand::Panic { hard: true });
        assert_eq!(p.press(t + Duration::from_secs(10)), ShowCommand::Panic { hard: false });
    }
}
