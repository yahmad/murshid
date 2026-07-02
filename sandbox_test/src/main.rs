fn main() {
        let mut s = String::from("hello");
        let r1 = &s;
        let r2 = &mut s; // Mutable borrow occurs
  here while immutable borrow is active
        println!("{}, {}", r1, r2);
    }
