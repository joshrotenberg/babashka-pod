//! A fixture for the outline tests.

use std::fmt;

pub const LIMIT: usize = 10;

static GREETING: &str = "hi";

pub type Pair = (i32, i32);

/// A point.
pub struct Point {
    pub x: i32,
    pub y: i32,
}

pub enum Shape {
    Circle(f64),
    Square(f64),
}

pub trait Area {
    fn area(&self) -> f64;
}

impl Area for Shape {
    fn area(&self) -> f64 {
        match self {
            Shape::Circle(r) => 3.14 * r * r,
            Shape::Square(s) => s * s,
        }
    }
}

impl fmt::Display for Point {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {})", self.x, self.y)
    }
}

pub fn add(
    a: i32,
    b: i32,
) -> i32 {
    a + b
}

mod inner {
    pub(crate) fn helper() {}
}

macro_rules! square {
    ($x:expr) => {
        $x * $x
    };
}
