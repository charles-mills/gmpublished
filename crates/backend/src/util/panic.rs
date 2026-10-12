#[must_use]
pub fn payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload.downcast_ref::<&str>().map_or_else(
        || {
            payload
                .downcast_ref::<String>()
                .map_or_else(|| "non-string panic payload".to_owned(), Clone::clone)
        },
        |message| (*message).to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::payload_message;

    #[test]
    fn both_payload_shapes_a_panic_can_carry_are_recovered() {
        let literal = std::panic::catch_unwind(|| panic!("a literal message"))
            .expect_err("the closure panics");
        assert_eq!(payload_message(literal.as_ref()), "a literal message");

        let formatted = std::panic::catch_unwind(|| panic!("formatted {}", 42))
            .expect_err("the closure panics");
        assert_eq!(payload_message(formatted.as_ref()), "formatted 42");

        let other = std::panic::catch_unwind(|| std::panic::panic_any(7_u8))
            .expect_err("the closure panics");
        assert_eq!(payload_message(other.as_ref()), "non-string panic payload");
    }
}
