fn id(x: usize) -> usize { x }
fn forward<T>(x: T) -> T { x }

struct Cell<T> { value: T }

impl<T> Cell<T> {
    fn new(value: T) -> Self { Self { value: value } }
    fn get_ref(&self) -> &T { &self.value }
}
