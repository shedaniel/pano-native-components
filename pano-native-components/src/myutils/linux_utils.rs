use x11rb::{
    connection::Connection,
    protocol::xproto::{AtomEnum, ConnectionExt, PropMode},
    wrapper::ConnectionExt as _,
};

pub fn apply_blur_to_window(window_id: u32) -> Result<(), Box<dyn std::error::Error>> {
    let (conn, _screen_num) = x11rb::connect(None)?;

    let atom = conn
        .intern_atom(false, b"_KDE_NET_WM_BLUR_BEHIND_REGION")?
        .reply()?
        .atom;

    let empty: [u32; 0] = [];

    let mut last_err: Option<Box<dyn std::error::Error>> = None;
    if conn.get_geometry(window_id)?.reply().is_ok() {
        // `.check()` forces a round trip so a BadWindow/protocol error
        // comes back as an Err instead of being silently dropped
        let result = conn
            .change_property32(
                PropMode::REPLACE,
                window_id,
                atom,
                AtomEnum::CARDINAL,
                &empty,
            )?
            .check();

        if result.is_ok() {
            conn.flush()?;
            return Ok(());
        }
        last_err = result.err().map(|e| e.into());
    }

    Err(last_err.unwrap_or_else(|| "failed to apply blur".into()))
}
