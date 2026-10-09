//! The Steam session runs in a helper process: this same executable started with `--steam-helper`.
//! Steam counts any process that initializes the Steam API with app ID 730 as CS2 until that process
//! exits, which would block launching the real game. The app therefore starts the helper only while
//! cs2.exe runs and ends it with the game. Requests and answers are JSON lines over stdin/stdout.
//!
//! The helper loads CS2's installed steam_api64.dll and only reads the friends API (recent co-players,
//! friends' current game and party lobby). It never touches the game process.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

pub const HELPER_ARG: &str = "--steam-helper";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CoplayEntry {
    #[serde(default)]
    pub steam_id: String,
    #[serde(default)]
    pub time: i64,
    #[serde(default)]
    pub game_id: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FriendInGame {
    pub steam_id: String,
    pub lobby: String,
    pub server: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Request {
    Init { file: String },
    Snapshot { since: i64 },
    Shutdown,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Response {
    ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(default)]
    entries: Vec<CoplayEntry>,
    #[serde(default)]
    friends: Vec<FriendInGame>,
}

pub struct Snapshot {
    pub entries: Vec<CoplayEntry>,
    pub friends: Vec<FriendInGame>,
}

// ---- main process side ----------------------------------------------------------------------------

struct Pipes {
    stdin: tokio::process::ChildStdin,
    stdout: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
}

/// The helper process. Dropping the session kills it.
pub struct SteamSession {
    child: tokio::process::Child,
    pipes: tokio::sync::Mutex<Option<Pipes>>,
}

impl SteamSession {
    pub async fn start(runtime: &Path) -> Result<Self, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut command = tokio::process::Command::new(exe);
        command
            .arg(HELPER_ARG)
            .env("SteamAppId", "730")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = command.spawn().map_err(|e| format!("Steam helper could not start: {e}"))?;
        let stdin = child.stdin.take().ok_or("Steam helper has no input")?;
        let stdout = child.stdout.take().ok_or("Steam helper has no output")?;
        use tokio::io::AsyncBufReadExt;
        let session = Self { child, pipes: tokio::sync::Mutex::new(Some(Pipes { stdin, stdout: tokio::io::BufReader::new(stdout).lines() })) };
        session.request(&Request::Init { file: runtime.to_string_lossy().into_owned() }).await?;
        Ok(session)
    }

    async fn request(&self, request: &Request) -> Result<Response, String> {
        use tokio::io::AsyncWriteExt;
        let mut guard = self.pipes.lock().await;
        let pipes = guard.as_mut().ok_or("Steam session is closed.")?;
        let exchange = async {
            let mut line = serde_json::to_string(request).map_err(|e| e.to_string())?;
            line.push('\n');
            pipes.stdin.write_all(line.as_bytes()).await.map_err(|e| e.to_string())?;
            pipes.stdin.flush().await.map_err(|e| e.to_string())?;
            let answer = pipes.stdout.next_line().await.map_err(|e| e.to_string())?.ok_or("Steam session ended unexpectedly.")?;
            serde_json::from_str::<Response>(&answer).map_err(|e| e.to_string())
        };
        let result = match tokio::time::timeout(REQUEST_TIMEOUT, exchange).await {
            Ok(result) => result,
            Err(_) => Err("Steam session did not respond.".into()),
        };
        match result {
            Ok(response) if response.ok => Ok(response),
            Ok(response) => Err(response.error.unwrap_or_else(|| "Steam request failed.".into())),
            Err(error) => {
                // A broken pipe or a hung helper cannot recover; the next CS2 start creates a new one.
                *guard = None;
                Err(error)
            }
        }
    }

    pub fn is_closed(&self) -> bool {
        self.pipes.try_lock().map(|pipes| pipes.is_none()).unwrap_or(false)
    }

    pub async fn snapshot(&self, since: i64) -> Result<Snapshot, String> {
        let response = self.request(&Request::Snapshot { since }).await?;
        Ok(Snapshot { entries: response.entries, friends: response.friends })
    }

    /// Ends the helper immediately (app exit).
    pub fn kill(&mut self) {
        let _ = self.child.start_kill();
    }

    /// Asks the helper to shut the Steam API down, then makes sure the process is gone.
    pub async fn shutdown(mut self) {
        if let Some(mut pipes) = self.pipes.lock().await.take() {
            use tokio::io::AsyncWriteExt;
            let _ = pipes.stdin.write_all(b"{\"type\":\"shutdown\"}\n").await;
            let _ = pipes.stdin.flush().await;
        }
        if tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await.is_err() {
            let _ = self.child.kill().await;
        }
    }
}

// ---- helper process side --------------------------------------------------------------------------

#[cfg(windows)]
mod runtime {
    use super::{CoplayEntry, FriendInGame};
    use libloading::{Library, Symbol};
    use std::ffi::{c_char, c_void, CStr};

    type Handle = *mut c_void;

    #[repr(C)]
    #[derive(Default)]
    struct FriendGameInfo {
        game_id: u64,
        game_ip: u32,
        game_port: u16,
        query_port: u16,
        lobby: u64,
    }

    const FRIEND_FLAG_IMMEDIATE: i32 = 4;

    pub struct Runtime {
        library: Library,
        friends: Handle,
    }

    impl Runtime {
        /// Loads and initializes the Steam API from CS2's own runtime.
        pub fn open(file: &str) -> Result<Self, String> {
            // SAFETY: steam_api64.dll is Valve's library from the CS2 install; loading it runs its initializers.
            let library = unsafe { Library::new(file) }.map_err(|e| format!("Installed Steam runtime could not be loaded: {e}"))?;
            let mut runtime = Self { library, friends: std::ptr::null_mut() };
            runtime.init()?;
            Ok(runtime)
        }

        fn symbol<T>(&self, name: &str) -> Result<Symbol<'_, T>, String> {
            // SAFETY: every call site states the exported function's C signature from Valve's steam_api_flat.h.
            unsafe { self.library.get::<T>(name.as_bytes()) }.map_err(|_| format!("Installed Steam runtime is incompatible: {name} is missing."))
        }

        fn init(&mut self) -> Result<(), String> {
            // Valve recommends InitFlat for dynamically loaded bindings; older runtimes only export Init.
            if let Ok(init_flat) = self.symbol::<unsafe extern "C" fn(*mut c_char) -> i32>("SteamAPI_InitFlat") {
                let mut message = [0 as c_char; 1024];
                // SAFETY: InitFlat writes a NUL-terminated message of at most 1024 bytes.
                let result = unsafe { init_flat(message.as_mut_ptr()) };
                if result != 0 {
                    // SAFETY: the buffer is NUL-terminated (zero-initialized and bounded by Steam).
                    let text = unsafe { CStr::from_ptr(message.as_ptr()) }.to_string_lossy().into_owned();
                    return Err(if text.is_empty() { format!("Steam initialization failed ({result}).") } else { text });
                }
            } else {
                let init = self.symbol::<unsafe extern "C" fn() -> bool>("SteamAPI_Init")?;
                // SAFETY: no arguments; returns whether the API initialized.
                if !unsafe { init() } {
                    return Err("Start Steam, sign in to an account that owns CS2, then restart CS2.".into());
                }
            }
            // Probe exported interface versions rather than assuming an SDK release; the newest wins.
            let accessor = (1..=100)
                .rev()
                .find_map(|version| self.symbol::<unsafe extern "C" fn() -> Handle>(&format!("SteamAPI_SteamFriends_v{version:03}")).ok().map(|f| *f))
                .ok_or("Installed Steam runtime has no compatible SteamFriends interface.")?;
            // SAFETY: the accessor takes no arguments and returns the ISteamFriends pointer (or null).
            self.friends = unsafe { accessor() };
            if self.friends.is_null() {
                return Err("SteamFriends interface is unavailable.".into());
            }
            Ok(())
        }

        pub fn snapshot(&self, since: i64) -> Result<(Vec<CoplayEntry>, Vec<FriendInGame>), String> {
            let run_callbacks = self.symbol::<unsafe extern "C" fn()>("SteamAPI_RunCallbacks")?;
            let count = self.symbol::<unsafe extern "C" fn(Handle) -> i32>("SteamAPI_ISteamFriends_GetCoplayFriendCount")?;
            let friend = self.symbol::<unsafe extern "C" fn(Handle, i32) -> u64>("SteamAPI_ISteamFriends_GetCoplayFriend")?;
            let time = self.symbol::<unsafe extern "C" fn(Handle, u64) -> i32>("SteamAPI_ISteamFriends_GetFriendCoplayTime")?;
            let game = self.symbol::<unsafe extern "C" fn(Handle, u64) -> u32>("SteamAPI_ISteamFriends_GetFriendCoplayGame")?;
            let name = self.symbol::<unsafe extern "C" fn(Handle, u64) -> *const c_char>("SteamAPI_ISteamFriends_GetFriendPersonaName")?;
            let mut entries = Vec::new();
            // SAFETY: all calls use the ISteamFriends pointer obtained in init and plain value arguments;
            // persona names are NUL-terminated strings owned by Steam and copied immediately.
            unsafe {
                run_callbacks();
                for index in 0..count(self.friends).max(0) {
                    let id = friend(self.friends, index);
                    let played = i64::from(time(self.friends, id));
                    let game_id = game(self.friends, id);
                    // Names are only needed for entries the roster can use.
                    let persona = if game_id == 730 && played >= since {
                        let pointer = name(self.friends, id);
                        if pointer.is_null() {
                            String::new()
                        } else {
                            CStr::from_ptr(pointer).to_string_lossy().into_owned()
                        }
                    } else {
                        String::new()
                    };
                    entries.push(CoplayEntry { steam_id: id.to_string(), time: played, game_id, name: persona, error: None });
                }
            }
            Ok((entries, self.friends_in_game(730).unwrap_or_default()))
        }

        /// Friends currently in the given app, with their server and party lobby. Optional: an older
        /// runtime without these exports gives an empty list.
        fn friends_in_game(&self, app_id: u64) -> Result<Vec<FriendInGame>, String> {
            let count = self.symbol::<unsafe extern "C" fn(Handle, i32) -> i32>("SteamAPI_ISteamFriends_GetFriendCount")?;
            let by_index = self.symbol::<unsafe extern "C" fn(Handle, i32, i32) -> u64>("SteamAPI_ISteamFriends_GetFriendByIndex")?;
            let played = self.symbol::<unsafe extern "C" fn(Handle, u64, *mut FriendGameInfo) -> bool>("SteamAPI_ISteamFriends_GetFriendGamePlayed")?;
            let mut result = Vec::new();
            // SAFETY: FriendGameInfo matches FriendGameInfo_t (8-byte packing: u64, u32, u16, u16, u64).
            unsafe {
                for index in 0..count(self.friends, FRIEND_FLAG_IMMEDIATE).max(0) {
                    let id = by_index(self.friends, index, FRIEND_FLAG_IMMEDIATE);
                    let mut info = FriendGameInfo::default();
                    // CGameID keeps the app ID in its low 24 bits.
                    if !played(self.friends, id, &mut info) || info.game_id & 0xff_ffff != app_id {
                        continue;
                    }
                    let server = if info.game_ip == 0 { String::new() } else { format!("{}:{}", info.game_ip, info.game_port) };
                    let lobby = if info.lobby == 0 { String::new() } else { info.lobby.to_string() };
                    result.push(FriendInGame { steam_id: id.to_string(), lobby, server });
                }
            }
            Ok(result)
        }
    }

    impl Drop for Runtime {
        fn drop(&mut self) {
            if let Ok(shutdown) = self.symbol::<unsafe extern "C" fn()>("SteamAPI_Shutdown") {
                // SAFETY: shuts down the API initialized in open.
                unsafe { shutdown() };
            }
        }
    }
}

/// Entry point of the helper process. Ends when the app closes its input or asks it to shut down.
pub fn run_helper() -> i32 {
    use std::io::{BufRead, Write};
    #[cfg(windows)]
    let mut runtime: Option<runtime::Runtime> = None;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(Request::Shutdown) => break,
            #[cfg(windows)]
            Ok(Request::Init { file }) => match runtime::Runtime::open(&file) {
                Ok(opened) => {
                    runtime = Some(opened);
                    Response { ok: true, ..Response::default() }
                }
                Err(error) => Response { error: Some(error), ..Response::default() },
            },
            #[cfg(windows)]
            Ok(Request::Snapshot { since }) => match runtime.as_ref().map(|r| r.snapshot(since)) {
                Some(Ok((entries, friends))) => Response { ok: true, entries, friends, ..Response::default() },
                Some(Err(error)) => Response { error: Some(error), ..Response::default() },
                None => Response { error: Some("Steam runtime is not initialized.".into()), ..Response::default() },
            },
            #[cfg(not(windows))]
            Ok(_) => Response { error: Some("The Steam helper only runs on Windows.".into()), ..Response::default() },
            Err(error) => Response { error: Some(error.to_string()), ..Response::default() },
        };
        let Ok(text) = serde_json::to_string(&response) else { break };
        if writeln!(stdout, "{text}").and_then(|_| stdout.flush()).is_err() {
            break;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_round_trips() {
        assert_eq!(serde_json::to_string(&Request::Snapshot { since: 5 }).unwrap(), r#"{"type":"snapshot","since":5}"#);
        assert!(matches!(serde_json::from_str::<Request>(r#"{"type":"shutdown"}"#).unwrap(), Request::Shutdown));
        let response: Response = serde_json::from_str(r#"{"ok":true,"entries":[{"steamId":"1","time":2,"gameId":730,"name":"x"}],"friends":[]}"#).unwrap();
        assert_eq!(response.entries[0].game_id, 730);
    }
}
