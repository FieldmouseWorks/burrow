//! Hold the retired locks until this deployed generation stops. The systemd
//! unit is ready only after migration and both locks have been acquired.
pub(super) fn hold() -> Result<(), String> {
    let path = std::env::var_os("NOTIFY_SOCKET").ok_or("--fence requires NOTIFY_SOCKET")?;
    burrow_systemd::notify(&path, "READY=1").map_err(|e| e.to_string())?;
    loop {
        std::thread::park();
    }
}
