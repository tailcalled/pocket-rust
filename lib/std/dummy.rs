pub fn id(x: usize) -> usize { x }
pub fn forward<T>(x: T) -> T { x }

pub struct Cell<T> { pub value: T }

impl<T> Cell<T> {
    pub fn new(value: T) -> Self { Self { value: value } }
    pub fn get_ref(&self) -> &T { &self.value }
}
