//! Virtual world inputs of one plugin, batched per tick. A batch is bounded
//! in items and bytes, whatever bots in the virtual world send. Over the
//! bounds, low-priority inputs (moves, plugin messages, settings, brand,
//! keep-alive times) are dropped first. Inputs that need an answer or end a
//! state (commands, chat, loaded, teleport confirmations, leaving) keep their
//! place in arrival order and wait for the next tick when the plugin cannot
//! take the batch (a full mailbox, a restart); leaving is never dropped.

use std::collections::{HashMap, VecDeque};

use pumbo_virtual::{Input, PlayerInput};

/// Inputs in one `on-virtual-input` call at most.
const MAX_BATCH_ITEMS: usize = 1024;
/// Payload bytes (chat, commands, brands, plugin messages) in one call at most.
const MAX_BATCH_BYTES: usize = 1024 * 1024;
/// Waiting priority inputs of one player at most; more are dropped.
const MAX_PRIORITY_PER_PLAYER: u32 = 16;
/// Waiting priority inputs of all players at most (leaving not counted)...
const MAX_PRIORITY: usize = 1024;
/// ...but each player may queue this many short ones (a `/login` typed by
/// hand) even when bots filled the rest.
const RESERVED_PER_PLAYER: u32 = 4;
const RESERVED_SIZE: usize = 256;
/// Moves of one player in a batch: a client sends about one per tick, a few
/// arrive together after a network stall. Over this, the newest move takes
/// the place of the player's last one while nothing else came after it.
const MAX_MOVED_PER_PLAYER: u32 = 8;

/// Inputs that need an answer or end a state.
pub(crate) fn priority(i: &Input) -> bool {
    matches!(
        i,
        Input::Command(_)
            | Input::Chat(_)
            | Input::Loaded
            | Input::TeleportConfirmed(_)
            | Input::Left
    )
}

fn size(i: &Input) -> usize {
    match i {
        Input::Chat(s) | Input::Command(s) | Input::Brand(s) => s.len(),
        Input::PluginMessage(c, d) => c.len() + d.len(),
        _ => 0,
    }
}

#[derive(Debug, Default)]
struct Counts {
    moved: u32,
    priority: u32,
    /// Sequence number of the player's last queued input when it is a move.
    last_moved: Option<u64>,
}

/// Inputs waiting for the next batch, in arrival order.
#[derive(Debug, Default)]
pub(crate) struct InputQueue {
    queue: VecDeque<PlayerInput>,
    /// Sequence number of `queue[0]`.
    first: u64,
    bytes: usize,
    priority: usize,
    priority_bytes: usize,
    players: HashMap<u64, Counts>,
    /// Inputs dropped since the caller last took the count.
    pub(crate) dropped: u64,
}

impl InputQueue {
    pub(crate) fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Queues an input or drops it (counted in `dropped`).
    pub(crate) fn push(&mut self, i: PlayerInput) {
        let size = size(&i.input);
        let seq = self.first + self.queue.len() as u64;
        let c = self.players.entry(i.player).or_default();
        let moved = matches!(i.input, Input::Moved { .. });
        if moved
            && c.moved >= MAX_MOVED_PER_PLAYER
            && let Some(slot) = c
                .last_moved
                .and_then(|at| usize::try_from(at.checked_sub(self.first)?).ok())
                .and_then(|at| self.queue.get_mut(at))
        {
            *slot = i;
            self.dropped += 1;
            return;
        }
        let counted = priority(&i.input) && i.input != Input::Left;
        let fits = if i.input == Input::Left {
            true
        } else if counted {
            c.priority < MAX_PRIORITY_PER_PLAYER
                && ((self.priority < MAX_PRIORITY && self.priority_bytes + size <= MAX_BATCH_BYTES)
                    || (c.priority < RESERVED_PER_PLAYER && size <= RESERVED_SIZE))
        } else {
            self.queue.len() < MAX_BATCH_ITEMS && self.bytes + size <= MAX_BATCH_BYTES
        };
        if !fits {
            self.dropped += 1;
            return;
        }
        if moved {
            c.moved += 1;
            c.last_moved = Some(seq);
        } else {
            c.last_moved = None;
        }
        if counted {
            c.priority += 1;
            self.priority += 1;
            self.priority_bytes += size;
        }
        self.bytes += size;
        self.queue.push_back(i);
    }

    /// The next batch, from the front of the queue, within the bounds of one
    /// call; the rest waits for the next tick.
    pub(crate) fn take(&mut self) -> Vec<PlayerInput> {
        let mut out = Vec::new();
        let mut bytes = 0;
        while let Some(front) = self.queue.front() {
            let s = size(&front.input);
            if !out.is_empty() && (out.len() >= MAX_BATCH_ITEMS || bytes + s > MAX_BATCH_BYTES) {
                break;
            }
            let Some(i) = self.queue.pop_front() else {
                break;
            };
            self.forget(&i, s, self.first);
            self.first += 1;
            bytes += s;
            out.push(i);
        }
        out
    }

    fn forget(&mut self, i: &PlayerInput, size: usize, seq: u64) {
        self.bytes -= size;
        let Some(c) = self.players.get_mut(&i.player) else {
            return;
        };
        if matches!(i.input, Input::Moved { .. }) {
            c.moved = c.moved.saturating_sub(1);
            if c.last_moved == Some(seq) {
                c.last_moved = None;
            }
        } else if priority(&i.input) && i.input != Input::Left {
            c.priority = c.priority.saturating_sub(1);
            self.priority -= 1;
            self.priority_bytes -= size;
        }
        if c.moved == 0 && c.priority == 0 && c.last_moved.is_none() {
            self.players.remove(&i.player);
        }
    }

    /// The plugin did not take `batch`: its priority inputs go back to the
    /// front for the next tick, the rest is dropped.
    pub(crate) fn give_back(&mut self, batch: Vec<PlayerInput>) {
        for i in batch.into_iter().rev() {
            if !priority(&i.input) {
                self.dropped += 1;
                continue;
            }
            let size = size(&i.input);
            if i.input != Input::Left {
                self.players.entry(i.player).or_default().priority += 1;
                self.priority += 1;
                self.priority_bytes += size;
            }
            self.bytes += size;
            // Taken from the front before, so `first` does not go below zero.
            self.first = self.first.saturating_sub(1);
            self.queue.push_front(i);
        }
    }

    /// Drops everything (the plugin is down until a reload).
    pub(crate) fn clear(&mut self) {
        let dropped = self.dropped + self.queue.len() as u64;
        *self = InputQueue {
            first: self.first + self.queue.len() as u64,
            dropped,
            ..InputQueue::default()
        };
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use pumbo_virtual::Position;

    fn moved(player: u64, x: f64) -> PlayerInput {
        PlayerInput {
            player,
            input: Input::Moved {
                position: Position {
                    x,
                    y: 64.0,
                    z: 0.0,
                    yaw: 0.0,
                    pitch: 0.0,
                },
                on_ground: true,
            },
        }
    }

    fn input(player: u64, input: Input) -> PlayerInput {
        PlayerInput { player, input }
    }

    /// A flood of 330 bots with plugin messages of 32 KB, moves and commands
    /// in one tick: the batch stays within its bounds, the login of a real
    /// player and every leave arrive in order, the rest is counted.
    #[test]
    fn flood_is_bounded_and_keeps_priority_inputs() {
        let mut q = InputQueue::default();
        q.push(input(1, Input::Loaded));
        for round in 0..50 {
            for bot in 100..430 {
                q.push(input(
                    bot,
                    Input::PluginMessage("x:y".into(), vec![0; 32 * 1024]),
                ));
                q.push(moved(bot, f64::from(round)));
                q.push(input(bot, Input::Command(format!("spam {round}"))));
            }
        }
        q.push(input(1, Input::Command("login secret".into())));
        for bot in 100..430 {
            q.push(input(bot, Input::Left));
        }
        let pushed = 1 + 50 * 330 * 3 + 1 + 330;
        let mut delivered = Vec::new();
        let mut ticks = 0;
        while !q.is_empty() {
            let batch = q.take();
            let bytes: usize = batch.iter().map(|i| size(&i.input)).sum();
            assert!(batch.len() <= MAX_BATCH_ITEMS, "{}", batch.len());
            assert!(bytes <= MAX_BATCH_BYTES, "{bytes}");
            delivered.extend(batch);
            ticks += 1;
        }
        assert!(ticks <= 4, "{ticks}");
        assert_eq!(delivered.len() as u64 + q.dropped, pushed as u64);
        let player1: Vec<&Input> = delivered
            .iter()
            .filter(|i| i.player == 1)
            .map(|i| &i.input)
            .collect();
        assert_eq!(
            player1,
            [&Input::Loaded, &Input::Command("login secret".into())]
        );
        let left = delivered.iter().filter(|i| i.input == Input::Left).count();
        assert_eq!(left, 330);
        // Every bot keeps a share of commands, none more than its limit.
        for bot in 100..430 {
            let commands = delivered
                .iter()
                .filter(|i| i.player == bot && matches!(i.input, Input::Command(_)))
                .count();
            assert!(
                (1..=MAX_PRIORITY_PER_PLAYER as usize).contains(&commands),
                "{bot}: {commands}"
            );
        }
    }

    #[test]
    fn priority_inputs_of_one_player_are_limited() {
        let mut q = InputQueue::default();
        for n in 0..100 {
            q.push(input(1, Input::Command(format!("spam {n}"))));
        }
        q.push(input(1, Input::Left));
        let batch = q.take();
        assert_eq!(batch.len(), MAX_PRIORITY_PER_PLAYER as usize + 1);
        assert_eq!(batch.last().unwrap().input, Input::Left);
        assert_eq!(q.dropped, 100 - u64::from(MAX_PRIORITY_PER_PLAYER));
    }

    /// Normal moves pass one by one (a check that simulates the fall needs
    /// each of them); over the limit the newest move replaces the last one in
    /// place, never before a teleport confirmation that came after it.
    #[test]
    fn moves_are_kept_until_the_limit_and_merged_in_place() {
        let mut q = InputQueue::default();
        q.push(moved(1, 0.0));
        q.push(moved(1, 1.0));
        q.push(moved(2, 0.0));
        let batch = q.take();
        assert_eq!(batch.len(), 3);
        assert_eq!(q.dropped, 0);

        for x in 0..MAX_MOVED_PER_PLAYER {
            q.push(moved(1, f64::from(x)));
        }
        q.push(input(1, Input::TeleportConfirmed(7)));
        q.push(moved(1, 100.0));
        q.push(moved(1, 101.0));
        q.push(moved(1, 102.0));
        let batch = q.take();
        let n = MAX_MOVED_PER_PLAYER as usize;
        assert_eq!(batch.len(), n + 2, "{batch:?}");
        assert_eq!(batch[n].input, Input::TeleportConfirmed(7));
        assert_eq!(batch[n + 1], moved(1, 102.0));
        assert_eq!(q.dropped, 2);
    }

    /// A batch the plugin could not take (full mailbox, restart): priority
    /// inputs come again first next tick, in order, before newer ones; the
    /// low-priority inputs of it are counted as dropped.
    #[test]
    fn priority_inputs_survive_a_failed_delivery() {
        let mut q = InputQueue::default();
        q.push(input(1, Input::Command("login secret".into())));
        q.push(moved(1, 0.0));
        q.push(input(1, Input::Left));
        q.push(input(2, Input::Settings));
        let batch = q.take();
        assert!(q.is_empty());
        q.push(input(3, Input::Chat("hi".into())));
        q.give_back(batch);
        assert_eq!(q.dropped, 2);
        let next: Vec<Input> = q.take().into_iter().map(|i| i.input).collect();
        assert_eq!(
            next,
            [
                Input::Command("login secret".into()),
                Input::Left,
                Input::Chat("hi".into())
            ]
        );
        // The counts are back to zero once everything left the queue.
        assert!(q.players.is_empty());
        assert_eq!((q.bytes, q.priority, q.priority_bytes), (0, 0, 0));
    }

    #[test]
    fn clear_counts_everything() {
        let mut q = InputQueue::default();
        q.push(moved(1, 0.0));
        q.push(input(1, Input::Left));
        q.clear();
        assert!(q.is_empty());
        assert_eq!(q.dropped, 2);
        q.push(moved(1, 0.0));
        assert_eq!(q.take(), [moved(1, 0.0)]);
    }
}
