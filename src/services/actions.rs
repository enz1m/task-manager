//! Process actions and their signal mappings.

/// The signals supported by owned process actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Signal {
    /// SIGTERM (15): a polite request to terminate; the process may refuse.
    Term,
    /// SIGKILL (9): unconditional termination; cannot be caught or ignored.
    Kill,
}

impl Signal {
    /// The exact signal value passed to `pidfd_send_signal`.
    pub(crate) fn as_rustix(self) -> rustix::process::Signal {
        match self {
            Signal::Term => rustix::process::Signal::TERM,
            Signal::Kill => rustix::process::Signal::KILL,
        }
    }
}

/// Destructive process verbs and their signal mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessAction {
    /// "End process (SIGTERM)" — the polite request.
    End,
    /// "Force kill (SIGKILL)" — the unconditional kill.
    ForceKill,
}

impl ProcessAction {
    /// The signal sent by this action.
    pub(crate) fn signal(self) -> Signal {
        match self {
            ProcessAction::End => Signal::Term,
            ProcessAction::ForceKill => Signal::Kill,
        }
    }
}
