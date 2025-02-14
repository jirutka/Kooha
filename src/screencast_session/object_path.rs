use gtk::glib::{self, translate::ToGlibPtr, FromVariant, StaticVariantType, ToVariant};

use std::borrow::Cow;

#[derive(Debug, PartialEq, Eq)]
pub struct ObjectPath(String);

impl StaticVariantType for ObjectPath {
    fn static_variant_type() -> Cow<'static, glib::VariantTy> {
        Cow::Borrowed(glib::VariantTy::OBJECT_PATH)
    }
}

impl FromVariant for ObjectPath {
    fn from_variant(value: &glib::Variant) -> Option<Self> {
        Self::new(value.get::<String>()?)
    }
}

impl ToVariant for ObjectPath {
    fn to_variant(&self) -> glib::Variant {
        // SAFETY: The only way to construct an `ObjectPath` is to go through
        // `new` function, which also verifies the input.
        unsafe {
            glib::translate::from_glib_none(glib::ffi::g_variant_new_object_path(
                self.0.to_glib_none().0,
            ))
        }
    }
}

impl ObjectPath {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let string = value.into();

        if !glib::Variant::is_object_path(&string) {
            tracing::warn!("Invalid object path `{}`", string);
            return None;
        }

        Some(Self(string))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_variant() {
        let o = ObjectPath::new("/com/example/Object").unwrap();
        assert_eq!(o.to_variant().type_(), glib::VariantTy::OBJECT_PATH);
    }

    #[test]
    fn to_variant_err() {
        let o = ObjectPath::new("invalidpath");
        assert!(o.is_none());
    }

    #[test]
    fn static_variant_type() {
        assert_eq!(
            ObjectPath::static_variant_type(),
            glib::VariantTy::OBJECT_PATH
        );
    }

    #[test]
    fn from_variant() {
        let variant_a = ObjectPath::new("/com/example/Object").unwrap().to_variant();
        assert_eq!(
            variant_a.get::<ObjectPath>().unwrap().as_str(),
            "/com/example/Object"
        );

        let variant_b = glib::Variant::parse(Some(glib::VariantTy::OBJECT_PATH), "'/foo'").unwrap();
        assert_eq!(variant_b.get::<String>().unwrap(), "/foo");
    }
}
