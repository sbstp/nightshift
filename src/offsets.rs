use std::{
    fmt,
    ops::{Add, AddAssign, Sub},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Absolute(u64);

impl Absolute {
    pub fn saturating_sub(self, other: Absolute) -> Relative {
        Relative(self.0.saturating_sub(other.0).try_into().expect("should fit"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Relative(usize);

impl Relative {
    pub fn min<O>(self, other: O) -> usize
    where
        O: TryInto<usize>,
        O::Error: fmt::Debug,
    {
        std::cmp::min(self.0, other.try_into().expect("should convert"))
    }
}

impl From<u64> for Absolute {
    fn from(value: u64) -> Self {
        Absolute(value)
    }
}

impl From<i64> for Absolute {
    fn from(value: i64) -> Self {
        Absolute(value.try_into().expect("positive offset"))
    }
}

impl From<i32> for Absolute {
    fn from(value: i32) -> Self {
        Absolute(value.try_into().expect("positive offset"))
    }
}

impl From<u32> for Absolute {
    fn from(value: u32) -> Self {
        Absolute(value.into())
    }
}

impl From<usize> for Relative {
    fn from(value: usize) -> Self {
        Relative(value)
    }
}

impl From<Absolute> for u64 {
    fn from(value: Absolute) -> Self {
        value.0
    }
}

impl From<Relative> for usize {
    fn from(value: Relative) -> Self {
        value.0
    }
}

impl Add<Relative> for Absolute {
    type Output = Absolute;

    fn add(self, rhs: Relative) -> Self::Output {
        Absolute(self.0 + u64::try_from(rhs.0).expect("usize should fit u64"))
    }
}

impl Add<Absolute> for Relative {
    type Output = Absolute;

    fn add(self, rhs: Absolute) -> Self::Output {
        rhs.add(self)
    }
}

impl AddAssign<u64> for Absolute {
    fn add_assign(&mut self, rhs: u64) {
        self.0 += rhs;
    }
}

impl AddAssign<usize> for Absolute {
    fn add_assign(&mut self, rhs: usize) {
        self.0.add_assign(&rhs.try_into().expect("should convert"));
    }
}
impl Sub for Absolute {
    type Output = Relative;

    fn sub(self, rhs: Self) -> Self::Output {
        // dbg!(self, rhs);
        Relative((self.0 - rhs.0).try_into().expect("should fit usize"))
    }
}

impl PartialEq<u64> for Absolute {
    fn eq(&self, other: &u64) -> bool {
        self.0 == *other
    }
}

impl PartialEq<Absolute> for u64 {
    fn eq(&self, other: &Absolute) -> bool {
        other == self
    }
}

impl fmt::Display for Absolute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for Relative {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
