//! Characters, animation and input (WP14): the input services, the Animator
//! and its tracks, Humanoid movement and state, rigs and seats.
//!
//! `cargo test -p re_compat character` selects these tests.

#[path = "character/place.rs"]
mod place;

mod character {
    #[path = "../character/anim.rs"]
    mod anim;
    #[path = "../character/humanoid.rs"]
    mod humanoid;
    #[path = "../character/input.rs"]
    mod input;
}
