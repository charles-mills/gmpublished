use std::hash::BuildHasher;

pub(super) type Map<K, V> = std::collections::HashMap<K, V, Builder>;
pub(super) type Set<K> = std::collections::HashSet<K, Builder>;

#[derive(Clone, Debug)]
pub(super) enum Builder {
    Fast(ahash::RandomState),
    #[cfg(any(test, feature = "map-capture"))]
    Standard(std::collections::hash_map::RandomState),
}
impl Default for Builder {
    fn default() -> Self {
        #[cfg(feature = "map-capture")]
        {
            static STANDARD: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if *STANDARD.get_or_init(|| {
                std::env::var_os("GMPUBLISHED_FAST_RESIDENCY_HASH").as_deref()
                    == Some(std::ffi::OsStr::new("0"))
            }) {
                return Self::Standard(std::collections::hash_map::RandomState::new());
            }
        }
        Self::Fast(ahash::RandomState::new())
    }
}
impl BuildHasher for Builder {
    type Hasher = Hasher;
    fn build_hasher(&self) -> Self::Hasher {
        match self {
            Self::Fast(builder) => Hasher::Fast(builder.build_hasher()),
            #[cfg(any(test, feature = "map-capture"))]
            Self::Standard(builder) => Hasher::Standard(builder.build_hasher()),
        }
    }
}
pub(super) enum Hasher {
    Fast(ahash::AHasher),
    #[cfg(any(test, feature = "map-capture"))]
    Standard(std::collections::hash_map::DefaultHasher),
}
macro_rules! forward {
    ($($method:ident($argument:ident: $ty:ty)),* $(,)?) => {$(
        fn $method(&mut self, $argument: $ty) {
            match self {
                Self::Fast(hasher) => hasher.$method($argument),
                #[cfg(any(test, feature = "map-capture"))]
                Self::Standard(hasher) => hasher.$method($argument),
            }
        }
    )*};
}
impl std::hash::Hasher for Hasher {
    fn finish(&self) -> u64 {
        match self {
            Self::Fast(hasher) => hasher.finish(),
            #[cfg(any(test, feature = "map-capture"))]
            Self::Standard(hasher) => hasher.finish(),
        }
    }
    forward!(
        write(bytes: &[u8]), write_u8(value: u8), write_u16(value: u16),
        write_u32(value: u32), write_u64(value: u64), write_u128(value: u128),
        write_usize(value: usize), write_i8(value: i8), write_i16(value: i16),
        write_i32(value: i32), write_i64(value: i64), write_i128(value: i128),
        write_isize(value: isize),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapper_preserves_native_integer_and_byte_hashing_for_each_algorithm() {
        let key = (
            3u8, 5u16, 8u32, 13u64, 21u128, 34usize, -3i8, -5i16, -8i32, -13i64, -21i128, -34isize,
        );
        let fast = ahash::RandomState::new();
        assert_eq!(
            Builder::Fast(fast.clone()).hash_one(key),
            fast.hash_one(key)
        );
        assert_eq!(
            Builder::Fast(fast.clone()).hash_one(b"texture"),
            fast.hash_one(b"texture")
        );
        let standard = std::collections::hash_map::RandomState::new();
        assert_eq!(
            Builder::Standard(standard.clone()).hash_one(key),
            standard.hash_one(key)
        );
        assert_eq!(
            Builder::Standard(standard.clone()).hash_one(b"texture"),
            standard.hash_one(b"texture")
        );
    }

    #[test]
    fn membership_and_replacement_are_independent_of_algorithm_seed_and_insertion_order() {
        for builder in [
            Builder::Fast(ahash::RandomState::new()),
            Builder::Fast(ahash::RandomState::new()),
            Builder::Standard(std::collections::hash_map::RandomState::new()),
        ] {
            for reverse in [false, true] {
                let mut map = Map::with_hasher(builder.clone());
                let mut set = Set::with_hasher(builder.clone());
                let mut entries = [(11u64, 100u64), (19, 200), (23, 300)];
                if reverse {
                    entries.reverse();
                }
                for (key, value) in entries {
                    map.insert(key, value);
                    set.insert(key);
                }
                assert_eq!(map.insert(19, 400), Some(200));
                assert_eq!(map.values().sum::<u64>(), 800);
                assert_eq!(map.get(&19), Some(&400));
                assert_eq!(set.len(), 3);
                assert!(set.contains(&11) && set.contains(&19) && set.contains(&23));
                assert!(!set.contains(&29));
            }
        }
    }
}
