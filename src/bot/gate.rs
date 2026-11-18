use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use tokio::sync::oneshot;

use crate::ids::UserId;

/// The update being handled for one user and those waiting behind it.
#[derive(Default)]
struct UserQueue {
    is_busy: bool,
    waiting: VecDeque<oneshot::Sender<()>>,
}

/// Runs each user's updates one at a time in arrival order, so the dispatcher
/// can handle different users concurrently. A queue exists only while the user
/// has updates in flight.
pub struct UserGate {
    max_pending: usize,
    queues: Mutex<HashMap<UserId, UserQueue>>,
}

impl UserGate {
    pub fn new(max_pending: usize) -> Arc<UserGate> {
        Arc::new(UserGate {
            max_pending,
            queues: Mutex::new(HashMap::new()),
        })
    }

    /// Takes a place in the user's queue, or `None` if the queue is full. The
    /// place is fixed here, so the order does not depend on task scheduling.
    pub fn admit(self: &Arc<Self>, user: UserId) -> Option<Admission> {
        let mut queues = self.queues();
        let queue = queues.entry(user).or_default();
        if usize::from(queue.is_busy) + queue.waiting.len() >= self.max_pending {
            return None;
        }
        let waiting = if queue.is_busy {
            let (sender, receiver) = oneshot::channel();
            queue.waiting.push_back(sender);
            Some(receiver)
        } else {
            queue.is_busy = true;
            None
        };
        Some(Admission {
            gate: Arc::clone(self),
            user,
            waiting,
        })
    }

    fn queues(&self) -> MutexGuard<'_, HashMap<UserId, UserQueue>> {
        // The map stays consistent: no user code runs under the lock.
        self.queues.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn release(&self, user: UserId, holds_turn: bool) {
        let mut queues = self.queues();
        let Some(queue) = queues.get_mut(&user) else {
            return;
        };
        // Places of cancelled updates must not count as pending.
        queue.waiting.retain(|sender| !sender.is_closed());
        if holds_turn {
            queue.is_busy = false;
            while let Some(next) = queue.waiting.pop_front() {
                if next.send(()).is_ok() {
                    queue.is_busy = true;
                    break;
                }
            }
        }
        if !queue.is_busy && queue.waiting.is_empty() {
            queues.remove(&user);
        }
    }
}

/// A place in a user's queue; dropping it hands the turn to the next update.
pub struct Admission {
    gate: Arc<UserGate>,
    user: UserId,
    waiting: Option<oneshot::Receiver<()>>,
}

impl Admission {
    pub async fn wait_turn(&mut self) {
        if let Some(receiver) = &mut self.waiting {
            // The sender is dropped only together with the gate's queue entry.
            let _ = receiver.await;
            self.waiting = None;
        }
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        // The turn may have been handed over after the last poll.
        let holds_turn = match self.waiting.take() {
            Some(mut receiver) => receiver.try_recv().is_ok(),
            None => true,
        };
        self.gate.release(self.user, holds_turn);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::{task, time};

    use super::*;

    const USER: UserId = UserId(1);

    fn queue_count(gate: &UserGate) -> usize {
        gate.queues().len()
    }

    async fn assert_has_turn(admission: &mut Admission) {
        time::timeout(Duration::from_secs(5), admission.wait_turn())
            .await
            .expect("admission must hold the turn");
    }

    #[tokio::test]
    async fn updates_run_in_arrival_order() {
        let gate = UserGate::new(8);
        let log = Arc::new(Mutex::new(Vec::new()));
        let admissions: Vec<Admission> = (0..4).map(|_| gate.admit(USER).unwrap()).collect();
        let mut tasks = Vec::new();
        // Spawned last to first: the order comes from `admit`, not from
        // polling.
        for (number, mut admission) in admissions.into_iter().enumerate().rev() {
            let log = Arc::clone(&log);
            tasks.push(tokio::spawn(async move {
                admission.wait_turn().await;
                task::yield_now().await;
                log.lock().unwrap().push(number);
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(*log.lock().unwrap(), [0, 1, 2, 3]);
    }

    #[tokio::test]
    async fn updates_over_limit_are_refused_per_user() {
        let gate = UserGate::new(2);
        let first = gate.admit(USER).unwrap();
        let _second = gate.admit(USER).unwrap();
        assert!(gate.admit(USER).is_none());
        assert!(gate.admit(UserId(2)).is_some());
        drop(first);
        assert!(gate.admit(USER).is_some());
    }

    #[tokio::test]
    async fn idle_users_leave_no_entries() {
        let gate = UserGate::new(4);
        let mut first = gate.admit(USER).unwrap();
        let mut second = gate.admit(USER).unwrap();
        first.wait_turn().await;
        drop(first);
        assert_has_turn(&mut second).await;
        assert_eq!(queue_count(&gate), 1);
        drop(second);
        assert_eq!(queue_count(&gate), 0);
    }

    #[tokio::test]
    async fn cancelled_update_gives_up_its_place() {
        let gate = UserGate::new(2);
        let first = gate.admit(USER).unwrap();
        let cancelled = gate.admit(USER).unwrap();
        drop(cancelled);
        let mut next = gate.admit(USER).unwrap();
        drop(first);
        assert_has_turn(&mut next).await;
        drop(next);
        assert_eq!(queue_count(&gate), 0);
    }

    #[tokio::test]
    async fn turn_handed_to_cancelled_update_passes_on() {
        let gate = UserGate::new(3);
        let first = gate.admit(USER).unwrap();
        let handed_over = gate.admit(USER).unwrap();
        let mut last = gate.admit(USER).unwrap();
        drop(first);
        drop(handed_over);
        assert_has_turn(&mut last).await;
    }
}
