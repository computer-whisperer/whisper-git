use std::sync::mpsc::Receiver;

use crate::git::RemoteOpResult;

pub type TimedRemoteOp = (Receiver<RemoteOpResult>, std::time::Instant, String);
pub type GenericRemoteOp = (Receiver<RemoteOpResult>, String, std::time::Instant);

pub type TimedRemoteOpSlot = Option<TimedRemoteOp>;
pub type GenericRemoteOpSlot = Option<GenericRemoteOp>;
