//! The protocol pairs this crate translates.

use crate::version::{
    V1_21, V1_21_2, V1_21_4, V1_21_5, V1_21_6, V1_21_7, V1_21_9, V1_21_11, V26_1, V26_2, V26_3,
};

/// A client protocol this crate translates on a server protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pair {
    pub client: i32,
    pub server: i32,
    /// Not proven on live servers yet: a caller leaves the pair off until
    /// the owner turns it on.
    pub experimental: bool,
}

/// An older client on a 26.3 server, proven on live servers.
const fn stable(client: i32) -> Pair {
    Pair {
        client,
        server: V26_3,
        experimental: false,
    }
}

/// The pairs this crate's code translates directly. A pair is listed when
/// its code and its data (`data/`) are here, so a new pair is a row, its
/// code and its data.
pub const PAIRS: &[Pair] = &[
    stable(V1_21),
    stable(V1_21_2),
    stable(V1_21_4),
    stable(V1_21_5),
    stable(V1_21_6),
    stable(V1_21_7),
    stable(V1_21_9),
    stable(V1_21_11),
    stable(V26_1),
    stable(V26_2),
];

/// What this crate does with `client` on `server`: the pair with its mark,
/// or `None` when it does not translate it. The answer is a value, not a
/// place in [`PAIRS`].
pub fn find(client: i32, server: i32) -> Option<Pair> {
    PAIRS
        .iter()
        .find(|p| p.client == client && p.server == server)
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Clients 1.21 to 26.2 on 26.3, none experimental; a new pair is added
    /// here on purpose.
    #[test]
    fn the_pairs_are_todays_ten_in_order() {
        let today: Vec<Pair> = (767..=776)
            .map(|client| Pair {
                client,
                server: 777,
                experimental: false,
            })
            .collect();
        assert_eq!(PAIRS, today.as_slice());
    }

    #[test]
    fn find_knows_exactly_the_listed_pairs() {
        assert_eq!(
            find(769, 777),
            Some(Pair {
                client: 769,
                server: 777,
                experimental: false,
            })
        );
        for (client, server) in [(777, 777), (777, 776), (766, 777), (769, 776)] {
            assert_eq!(find(client, server), None, "{client} on {server}");
        }
    }
}
