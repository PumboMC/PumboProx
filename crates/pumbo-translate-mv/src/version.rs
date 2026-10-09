//! Protocol numbers and encoding eras.

pub(crate) const V1_21: i32 = 767;
pub(crate) const V1_21_2: i32 = 768;
pub(crate) const V1_21_4: i32 = 769;
pub(crate) const V1_21_5: i32 = 770;
pub(crate) const V1_21_6: i32 = 771;
pub(crate) const V1_21_9: i32 = 773;
pub(crate) const V1_21_11: i32 = 774;
pub(crate) const V26_1: i32 = 775;
pub(crate) const V26_2: i32 = 776;
pub(crate) const V26_3: i32 = 777;

/// The server protocol whose packets this crate reads.
pub const SERVER_PROTOCOL: i32 = V26_3;
/// Oldest client protocol translated.
pub const OLDEST_CLIENT: i32 = V1_21;

/// A component's encoding eras (from pumpkin-java-multiversion). Each variant
/// starts at its protocol and lasts until the next one, so protocols where the
/// component did not change share a variant. `of` is the only comparison.
macro_rules! eras {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $($(#[$variant_meta:meta])* $variant:ident = $start:ident),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
        $vis enum $name {
            $($(#[$variant_meta])* $variant),+
        }

        impl $name {
            #[allow(unused_assignments, dead_code)]
            $vis fn of(protocol: i32) -> Self {
                let mut era = [$(Self::$variant),+][0];
                $(
                    if protocol >= $crate::version::$start {
                        era = Self::$variant;
                    }
                )+
                era
            }
        }
    };
}

pub(crate) use eras;

#[cfg(test)]
mod tests {
    eras! {
        enum Sample {
            A = V1_21,
            B = V1_21_5,
            C = V26_3,
        }
    }

    #[test]
    fn eras_cover_ranges() {
        assert_eq!(Sample::of(767), Sample::A);
        assert_eq!(Sample::of(769), Sample::A);
        assert_eq!(Sample::of(770), Sample::B);
        assert_eq!(Sample::of(776), Sample::B);
        assert_eq!(Sample::of(777), Sample::C);
    }
}
