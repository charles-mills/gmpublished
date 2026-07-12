use std::fmt;

/// A labeled value for `pick_list` widgets; displays as `label`, compares and
/// submits as `value`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectOption {
    pub(crate) label: String,
    pub(crate) value: String,
}

impl SelectOption {
    pub(crate) fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

impl fmt::Display for SelectOption {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}
