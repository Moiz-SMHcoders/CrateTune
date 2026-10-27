use gpui::{Fill, Styled};

pub trait AdditionalStyleUtil {
    fn background_opacity(self, opacity: f32) -> Self;
}

impl<T> AdditionalStyleUtil for T
where
    T: Styled,
{
    fn background_opacity(mut self, opacity: f32) -> Self {
        if let Some(v) = &mut self.style().background {
            match v {
                Fill::Color(hsla) => {
                    *v = Fill::Color(hsla.opacity(opacity));
                }
            }
        }

        self
    }
}
