day_bridge::bridge! {
    #[day_bridge::declare]
    extern "day" {
        fn request_native(request: &str, done: day_bridge::Done<String>) -> Result<(), day_bridge::Error>;
        fn watch_native(emit: day_bridge::Emit<String>) -> Result<(), day_bridge::Error>;
        fn cancel_native(token: i64) -> Result<(), day_bridge::Error>;
        fn unwatch_native(token: i64) -> Result<(), day_bridge::Error>;
    }
    #[day_bridge::impl(swift, platforms = [ios, macos], src = "platform/Bluetooth.swift")]
    #[day_bridge::impl(java, platforms = [android], src = "platform/Bluetooth.java")]
    #[day_bridge::impl(rust, platforms = [linux, windows])]
    fn request_native(request: &str, done: day_bridge::Done<String>) -> Result<(), day_bridge::Error> {
        crate::desktop::request(request, done)
    }
    #[day_bridge::impl(rust, platforms = [linux, windows])]
    fn watch_native(emit: day_bridge::Emit<String>) -> Result<(), day_bridge::Error> {
        crate::desktop::watch(emit)
    }
    #[day_bridge::impl(rust, platforms = [linux, windows])]
    fn cancel_native(token: i64) -> Result<(), day_bridge::Error> {
        crate::desktop::cancel(token as u64);
        Ok(())
    }
    #[day_bridge::impl(rust, platforms = [linux, windows])]
    fn unwatch_native(token: i64) -> Result<(), day_bridge::Error> {
        crate::desktop::unwatch(token as u64);
        Ok(())
    }
    #[day_bridge::impl(rust, platforms = [other])]
    fn request_native(_request: &str, _done: day_bridge::Done<String>) -> Result<(), day_bridge::Error> { Err(day_bridge::Error::Unsupported) }
    #[day_bridge::impl(rust, platforms = [other])]
    fn watch_native(_emit: day_bridge::Emit<String>) -> Result<(), day_bridge::Error> { Err(day_bridge::Error::Unsupported) }
    #[day_bridge::impl(rust, platforms = [other])]
    fn cancel_native(_token: i64) -> Result<(), day_bridge::Error> { Ok(()) }
    #[day_bridge::impl(rust, platforms = [other])]
    fn unwatch_native(_token: i64) -> Result<(), day_bridge::Error> { Ok(()) }
}

pub(crate) fn unwatch(token: i64) -> Result<(), day_bridge::Error> {
    unwatch_native(token)
}

pub(crate) fn cancel(token: i64) -> Result<(), day_bridge::Error> {
    cancel_native(token)
}
