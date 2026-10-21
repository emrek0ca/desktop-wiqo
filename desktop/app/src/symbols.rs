//! The iPhone app's icons are SF Symbols. On macOS the window draws the very same symbols: the system renders one (name, point size, weight)
//! into a PNG that the page uses as a mask, so the icon takes the text colour. Nothing is bundled; other systems use the page's SVG fallbacks.

#[cfg(target_os = "macos")]
pub fn png(name: &str, points: f64, weight: f64) -> Option<Vec<u8>> {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::{NSData, NSDictionary, NSString};

    if name.is_empty()
        || name.len() > 80
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
    {
        return None;
    }
    unsafe {
        let symbol = NSString::from_str(name);
        let image: Option<Retained<AnyObject>> = msg_send![class!(NSImage), imageWithSystemSymbolName: &*symbol, accessibilityDescription: std::ptr::null::<AnyObject>()];
        let image = image?;
        // Drawn at twice the size, shown at half: sharp on Retina screens.
        let config: Option<Retained<AnyObject>> = msg_send![class!(NSImageSymbolConfiguration), configurationWithPointSize: points * 2.0, weight: weight];
        let sized: Option<Retained<AnyObject>> =
            msg_send![&*image, imageWithSymbolConfiguration: &*config?];
        let sized = sized?;
        let tiff: Option<Retained<NSData>> = msg_send![&*sized, TIFFRepresentation];
        let rep: Option<Retained<AnyObject>> =
            msg_send![class!(NSBitmapImageRep), imageRepWithData: &*tiff?];
        let properties = NSDictionary::<AnyObject, AnyObject>::new();
        // NSBitmapImageFileTypePNG = 4
        let png: Option<Retained<NSData>> =
            msg_send![&*rep?, representationUsingType: 4usize, properties: &*properties];
        Some(png?.to_vec())
    }
}

#[cfg(not(target_os = "macos"))]
pub fn png(_name: &str, _points: f64, _weight: f64) -> Option<Vec<u8>> {
    None
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn the_system_draws_a_symbol_and_refuses_odd_names() {
        let png = super::png("magnifyingglass", 16.0, 0.0).expect("symbol");
        assert_eq!(&png[1..4], b"PNG");
        assert!(super::png("../etc", 16.0, 0.0).is_none());
        assert!(super::png("no.such.symbol.name", 16.0, 0.0).is_none());
    }
}
