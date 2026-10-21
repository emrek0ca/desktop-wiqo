//! "Where am I" for the weather: one location fix, turned into a city name on this computer. Only the city goes to the server (the backend
//! accepts nothing finer), exactly like the iPhone. macOS asks for the Location permission the first time; on other systems there is no
//! fix and the window asks the person to type the city.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Granted,
    Denied,
    NotDetermined,
    /// Location Services are off for the whole computer.
    Off,
}

// CLGeocoder is marked deprecated in favour of MapKit, which has no plain C/Objective-C path from here; it still works, and the iPhone
// app resolves the city the same way.
#[cfg(target_os = "macos")]
#[allow(deprecated)]
mod imp {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2_core_location::{
        kCLLocationAccuracyKilometer, CLAuthorizationStatus, CLGeocoder, CLLocation,
        CLLocationManager, CLPlacemark,
    };
    use objc2_foundation::{NSArray, NSDate, NSError, NSRunLoop};

    use super::Access;

    fn access_of(manager: &CLLocationManager) -> Access {
        if !unsafe { CLLocationManager::locationServicesEnabled_class() } {
            return Access::Off;
        }
        match unsafe { manager.authorizationStatus() } {
            CLAuthorizationStatus::AuthorizedAlways
            | CLAuthorizationStatus::AuthorizedWhenInUse => Access::Granted,
            CLAuthorizationStatus::NotDetermined => Access::NotDetermined,
            _ => Access::Denied,
        }
    }

    pub fn access() -> Access {
        let manager = unsafe { CLLocationManager::new() };
        access_of(&manager)
    }

    /// Turns the run loop of this thread for a moment: Core Location delivers to the thread that made the manager.
    fn spin(seconds: f64) {
        let until = NSDate::dateWithTimeIntervalSinceNow(seconds);
        NSRunLoop::currentRunLoop().runUntilDate(&until);
    }

    fn fix(limit: Duration) -> Option<Retained<CLLocation>> {
        let manager = unsafe { CLLocationManager::new() };
        unsafe { manager.setDesiredAccuracy(kCLLocationAccuracyKilometer) };
        let deadline = Instant::now() + limit;
        if access_of(&manager) == Access::NotDetermined {
            // The system dialog; the answer arrives while the run loop turns.
            unsafe { manager.requestWhenInUseAuthorization() };
            while access_of(&manager) == Access::NotDetermined && Instant::now() < deadline {
                spin(0.25);
            }
        }
        if access_of(&manager) != Access::Granted {
            return None;
        }
        unsafe { manager.startUpdatingLocation() };
        let mut found = None;
        while Instant::now() < deadline {
            spin(0.25);
            if let Some(location) = unsafe { manager.location() } {
                found = Some(location);
                break;
            }
        }
        unsafe { manager.stopUpdatingLocation() };
        found
    }

    fn city_of(location: &CLLocation, limit: Duration) -> Option<String> {
        let (send, receive) = mpsc::channel::<Option<String>>();
        let handler = RcBlock::new(
            move |places: *mut NSArray<CLPlacemark>, _error: *mut NSError| {
                let city = unsafe { places.as_ref() }
                    .and_then(|places| places.firstObject())
                    .and_then(|place: Retained<CLPlacemark>| unsafe {
                        place
                            .locality()
                            .or_else(|| place.subAdministrativeArea())
                            .or_else(|| place.administrativeArea())
                    })
                    .map(|name| name.to_string());
                let _ = send.send(city);
            },
        );
        let geocoder = unsafe { CLGeocoder::new() };
        unsafe {
            geocoder.reverseGeocodeLocation_completionHandler(location, RcBlock::as_ptr(&handler))
        };
        // The answer comes on the main thread's run loop (the app's own), not this one.
        receive.recv_timeout(limit).ok().flatten()
    }

    pub fn current_city(limit: Duration) -> Option<String> {
        let started = Instant::now();
        let location = fix(limit)?;
        let left = limit
            .saturating_sub(started.elapsed())
            .max(Duration::from_secs(3));
        city_of(&location, left)
            .map(|city| city.trim().to_string())
            .filter(|city| !city.is_empty())
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::time::Duration;

    use super::Access;

    pub fn access() -> Access {
        Access::Off
    }

    pub fn current_city(_limit: Duration) -> Option<String> {
        None
    }
}

/// Whether Wiqo may read the location (never shows a dialog).
pub fn access() -> Access {
    imp::access()
}

/// The city this computer is in, asking for the permission first when it was never asked. `None` when refused, off or not found in time.
/// Blocks (call it off the UI thread).
pub fn current_city(limit: Duration) -> Option<String> {
    imp::current_city(limit)
}
