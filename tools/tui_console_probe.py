"""Drive the full-screen Jani UI through a real Windows console and audit frames.

The probe allocates a console, binds the child's stdin/stdout/stderr to it and
injects key and mouse records with WriteConsoleInputW. It reads the active
screen buffer, so it checks rendered text rather than piped output. A local fake
OpenAI-compatible provider proves that a turn reaches the model gateway without
exposing the key.

Usage:
    python tools/tui_console_probe.py target/release/jani.exe target/tui-run

Windows only. It does not replace a terminal-emulator test: paste, scrollback,
color fidelity and rendering on Linux or macOS remain manual.
"""

import ctypes
import json
import msvcrt
import os
import shutil
import subprocess
import sys
import threading
import time
from ctypes import wintypes
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.stdout.reconfigure(encoding="utf-8", errors="replace")

kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
user32 = ctypes.WinDLL("user32", use_last_error=True)


class COORD(ctypes.Structure):
    _fields_ = [("X", wintypes.SHORT), ("Y", wintypes.SHORT)]


class SMALL_RECT(ctypes.Structure):
    _fields_ = [
        ("Left", wintypes.SHORT),
        ("Top", wintypes.SHORT),
        ("Right", wintypes.SHORT),
        ("Bottom", wintypes.SHORT),
    ]

GENERIC_READ = 0x80000000
GENERIC_WRITE = 0x40000000
FILE_SHARE_READ = 0x00000001
FILE_SHARE_WRITE = 0x00000002
OPEN_EXISTING = 3
DUPLICATE_SAME_ACCESS = 0x00000002
ENABLE_VIRTUAL_TERMINAL_PROCESSING = 0x0004
KEY_EVENT = 1
MOUSE_EVENT = 2
# dwEventFlags in a MOUSE_EVENT_RECORD: 0x0004 is the vertical wheel and 0x0008
# is the horizontal one. SendInput's MOUSEEVENTF_FROMWHEEL (0x08) is a different
# encoding and does not work here.
MOUSE_WHEELED = 0x0004
ENABLE_MOUSE_INPUT = 0x0010
ENABLE_EXTENDED_FLAGS = 0x0080
VK_RETURN = 0x0D
VK_ESCAPE = 0x1B
VK_F1 = 0x70
SHIFT_PRESSED = 0x0010
VK_PAGE_UP = 0x21
VK_DOWN = 0x28

kernel32.GetCurrentProcess.restype = wintypes.HANDLE
kernel32.CreateFileW.restype = wintypes.HANDLE
kernel32.CreateFileW.argtypes = [
    wintypes.LPCWSTR,
    wintypes.DWORD,
    wintypes.DWORD,
    wintypes.LPVOID,
    wintypes.DWORD,
    wintypes.DWORD,
    wintypes.HANDLE,
]
kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
kernel32.GetStdHandle.restype = wintypes.HANDLE
kernel32.GetStdHandle.argtypes = [wintypes.DWORD]
kernel32.DuplicateHandle.argtypes = [
    wintypes.HANDLE,
    wintypes.HANDLE,
    wintypes.HANDLE,
    ctypes.POINTER(wintypes.HANDLE),
    wintypes.DWORD,
    wintypes.BOOL,
    wintypes.DWORD,
]
kernel32.ReadConsoleOutputCharacterW.argtypes = [
    wintypes.HANDLE,
    wintypes.LPWSTR,
    wintypes.DWORD,
    COORD,
    ctypes.POINTER(wintypes.DWORD),
]
kernel32.SetConsoleScreenBufferSize.argtypes = [wintypes.HANDLE, COORD]
kernel32.SetConsoleWindowInfo.argtypes = [
    wintypes.HANDLE,
    wintypes.BOOL,
    ctypes.POINTER(SMALL_RECT),
]
kernel32.WriteConsoleInputW.argtypes = [
    wintypes.HANDLE,
    wintypes.LPVOID,
    wintypes.DWORD,
    ctypes.POINTER(wintypes.DWORD),
]


class CHAR_UNION(ctypes.Union):
    _fields_ = [("UnicodeChar", wintypes.WCHAR), ("AsciiChar", ctypes.c_char)]


class KEY_EVENT_RECORD(ctypes.Structure):
    _fields_ = [
        ("bKeyDown", wintypes.BOOL),
        ("wRepeatCount", wintypes.WORD),
        ("wVirtualKeyCode", wintypes.WORD),
        ("wVirtualScanCode", wintypes.WORD),
        ("uChar", CHAR_UNION),
        ("dwControlKeyState", wintypes.DWORD),
    ]


class MOUSE_EVENT_RECORD(ctypes.Structure):
    _fields_ = [
        ("dwMousePosition", COORD),
        ("dwButtonState", wintypes.DWORD),
        ("dwControlKeyState", wintypes.DWORD),
        # dwEventFlags is a WORD in Win32. Declaring it wider shifts the value
        # into an offset the reader interprets as a horizontal wheel event.
        ("dwEventFlags", wintypes.WORD),
    ]


class EVENT_UNION(ctypes.Union):
    _fields_ = [("KeyEvent", KEY_EVENT_RECORD), ("MouseEvent", MOUSE_EVENT_RECORD)]


class INPUT_RECORD(ctypes.Structure):
    _fields_ = [("EventType", wintypes.WORD), ("Event", EVENT_UNION)]


def fail(message):
    error = ctypes.get_last_error()
    print(f"FAIL: {message} (winerror={error})")
    sys.exit(2)


def duplicate(handle):
    current = kernel32.GetCurrentProcess()
    duplicate_handle = wintypes.HANDLE()
    if not kernel32.DuplicateHandle(
        current,
        handle,
        current,
        ctypes.byref(duplicate_handle),
        0,
        True,
        DUPLICATE_SAME_ACCESS,
    ):
        fail("DuplicateHandle")
    return duplicate_handle


class CONSOLE_SCREEN_BUFFER_INFO(ctypes.Structure):
    _fields_ = [
        ("dwSize", COORD),
        ("dwCursorPosition", COORD),
        ("wAttributes", wintypes.WORD),
        ("srWindow", SMALL_RECT),
        ("dwMaximumWindowSize", COORD),
    ]


kernel32.GetConsoleScreenBufferInfo.argtypes = [
    wintypes.HANDLE,
    ctypes.POINTER(CONSOLE_SCREEN_BUFFER_INFO),
]
GEOMETRY = None


def read_screen(width, height):
    handle = kernel32.CreateFileW(
        "CONOUT$",
        GENERIC_READ | GENERIC_WRITE,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        None,
        OPEN_EXISTING,
        0,
        None,
    )
    if handle == wintypes.HANDLE(-1).value:
        fail("open CONOUT$ for reading")
    # The host can resize the buffer after the probe sets its initial geometry,
    # so trust the live buffer stride rather than the requested width.
    info = CONSOLE_SCREEN_BUFFER_INFO()
    if not kernel32.GetConsoleScreenBufferInfo(handle, ctypes.byref(info)):
        kernel32.CloseHandle(handle)
        fail("GetConsoleScreenBufferInfo")
    global GEOMETRY
    stride = info.dwSize.X
    top = info.srWindow.Top
    window_width = info.srWindow.Right - info.srWindow.Left + 1
    window_height = info.srWindow.Bottom - info.srWindow.Top + 1
    geometry = (stride, info.dwSize.Y, window_width, window_height, top)
    if geometry != GEOMETRY:
        GEOMETRY = geometry
        print(
            f"console_geometry: buffer={stride}x{info.dwSize.Y}"
            f" window={window_width}x{window_height} top={top}"
        )
    cols = min(stride, window_width if window_width > 0 else width)
    rows = max(0, min(height, window_height, info.dwSize.Y - top))
    buffer = ctypes.create_unicode_buffer(stride * max(rows, 1))
    read = wintypes.DWORD()
    if not kernel32.ReadConsoleOutputCharacterW(
        handle, buffer, stride * rows, COORD(0, top), ctypes.byref(read)
    ):
        kernel32.CloseHandle(handle)
        fail("ReadConsoleOutputCharacterW")
    kernel32.CloseHandle(handle)
    text = buffer[: read.value]
    return "\n".join(
        text[row * stride : (row + 1) * stride][:cols].rstrip()
        for row in range(read.value // stride)
    )


def wait_for(width, height, needle, timeout=12.0):
    deadline = time.time() + timeout
    screen = ""
    while time.time() < deadline:
        screen = read_screen(width, height)
        if needle in screen:
            return screen
        time.sleep(0.4)
    return screen


def stable_screen(width, height, needle, timeout=12.0):
    # The idle UI redraws every 250 ms; a single read can land mid-diff. Two
    # identical consecutive frames are the settled content.
    deadline = time.time() + timeout
    screen = ""
    while time.time() < deadline:
        first = read_screen(width, height)
        if needle in first:
            time.sleep(0.3)
            second = read_screen(width, height)
            screen = second
            if first == second:
                return second
        else:
            screen = first
        time.sleep(0.15)
    return screen


def wait_until_idle(width, height, needle, timeout=25.0):
    # A streamed turn redraws continuously, so a settled frame is not enough: the
    # authority line only returns once the turn has finished.
    deadline = time.time() + timeout
    screen = ""
    while time.time() < deadline:
        first = read_screen(width, height)
        screen = first
        if needle in first and "writes off" in first:
            time.sleep(0.3)
            second = read_screen(width, height)
            screen = second
            if first == second:
                return second
        time.sleep(0.15)
    return screen


def key(handle, virtual_key=0, char="\x00", control_state=0):
    record = INPUT_RECORD()
    record.EventType = KEY_EVENT
    record.Event.KeyEvent.bKeyDown = 1
    record.Event.KeyEvent.wRepeatCount = 1
    record.Event.KeyEvent.wVirtualKeyCode = virtual_key
    record.Event.KeyEvent.uChar.UnicodeChar = char
    record.Event.KeyEvent.dwControlKeyState = control_state
    write_records(handle, (record,))


def type_text(handle, text):
    records = []
    for ch in text:
        record = INPUT_RECORD()
        record.EventType = KEY_EVENT
        record.Event.KeyEvent.bKeyDown = 1
        record.Event.KeyEvent.wRepeatCount = 1
        record.Event.KeyEvent.wVirtualKeyCode = VK_RETURN if ch == "\r" else 0
        record.Event.KeyEvent.uChar.UnicodeChar = ch
        records.append(record)
    # One call per string: the app reads one record per animation tick, so a
    # burst stays in the buffer until it is consumed.
    write_records(handle, records)


def write_records(handle, records):
    array = (INPUT_RECORD * len(records))(*records)
    written = wintypes.DWORD()
    if not kernel32.WriteConsoleInputW(handle, array, len(records), ctypes.byref(written)):
        fail("WriteConsoleInputW")
    if written.value != len(records):
        fail(f"WriteConsoleInputW wrote {written.value}/{len(records)}")


def enable_mouse(handle):
    """Set the mouse bit early so records are not dropped while the app starts."""
    mode = wintypes.DWORD()
    if not kernel32.GetConsoleMode(handle, ctypes.byref(mode)):
        fail("GetConsoleMode")
    wanted = wintypes.DWORD(mode.value | ENABLE_EXTENDED_FLAGS | ENABLE_MOUSE_INPUT)
    if not kernel32.SetConsoleMode(handle, wanted):
        fail("SetConsoleMode")


def send_mouse(handle, column, row, button_state, flags):
    record = INPUT_RECORD()
    record.EventType = MOUSE_EVENT
    record.Event.MouseEvent.dwMousePosition = COORD(column, row)
    record.Event.MouseEvent.dwButtonState = button_state & 0xFFFFFFFF
    record.Event.MouseEvent.dwEventFlags = flags
    write_records(handle, (record,))


def wheel(handle, column, row, ticks):
    send_mouse(handle, column, row, ticks, MOUSE_WHEELED)


def click(handle, column, row):
    send_mouse(handle, column, row, 1, 0)
    send_mouse(handle, column, row, 0, 0)


def row_of(screen, needle):
    for index, line in enumerate(screen.split("\n")):
        if needle in line:
            return index
    return -1


def palette_body(screen, title="commands", footer="enter run"):
    rows = screen.split("\n")
    start = next(
        (index for index, row in enumerate(rows) if "┌" in row and title in row),
        None,
    )
    end = next((index for index, row in enumerate(rows) if footer in row), None)
    if start is None or end is None or end < start:
        return ""
    return "\n".join(rows[start : end + 1])


class FakeProvider(BaseHTTPRequestHandler):
    captured = []

    def send_sse(self, payload):
        self.wfile.write(b"data: " + json.dumps(payload).encode("utf-8") + b"\n\n")
        self.wfile.flush()

    def do_POST(self):
        length = int(self.headers.get("content-length", "0"))
        body = self.rfile.read(length)
        FakeProvider.captured.append(body.decode("utf-8", "replace"))
        first = len(FakeProvider.captured) == 1
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.end_headers()
        if first:
            time.sleep(0.9)
            self.send_sse(
                {
                    "choices": [
                        {
                            "delta": {
                                "role": "assistant",
                                "content": "",
                                "tool_calls": [
                                    {
                                        "index": index,
                                        "id": f"call_probe_{index}",
                                        "type": "function",
                                        "function": {
                                            "name": "read",
                                            "arguments": '{"path":"notes.txt"}',
                                        },
                                    }
                                    for index in range(5)
                                ],
                            }
                        }
                    ]
                }
            )
            self.send_sse({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]})
            self.send_sse(
                {"choices": [], "usage": {"prompt_tokens": 18, "completion_tokens": 6}}
            )
        else:
            time.sleep(0.3)
            self.send_sse(
                {
                    "choices": [
                        {"delta": {"reasoning_content": "checking the fake provider\n"}}
                    ]
                }
            )
            for chunk in (
                "**Hello** from the `fake` provider.\n\n",
                "# | claim | verdict\n",
                "---|:---|---:\n",
                "1 | md | ok\n\n",
                "- alpha item\n",
                "- beta item",
            ):
                time.sleep(0.12)
                self.send_sse({"choices": [{"delta": {"content": chunk}}]})
            self.send_sse(
                {"choices": [], "usage": {"prompt_tokens": 24, "completion_tokens": 12}}
            )
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

    def log_message(self, *_args):
        pass


def main():
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    binary = os.path.abspath(sys.argv[1])
    version = subprocess.run(
        [binary, "--version"], capture_output=True, text=True, timeout=30
    )
    version_lines = version.stdout.strip().splitlines()
    if version.returncode != 0 or not version_lines or not version_lines[0].startswith("jani "):
        print("cannot read the binary version:", version.stdout, version.stderr)
        return 2
    jani_version = "Jani v" + version_lines[0].removeprefix("jani ").strip()
    scratch = os.path.abspath(sys.argv[2])
    shutil.rmtree(scratch, ignore_errors=True)
    os.makedirs(os.path.join(scratch, ".jani"))
    with open(os.path.join(scratch, "notes.txt"), "w", encoding="utf-8") as handle:
        handle.write("probe notes\n")
    skill_dir = os.path.join(scratch, ".agents", "skills", "probe-skill")
    os.makedirs(skill_dir)
    with open(os.path.join(skill_dir, "SKILL.md"), "w", encoding="utf-8") as handle:
        handle.write(
            "---\n"
            "name: probe-skill\n"
            "description: Probe the styled skills listing in a real console.\n"
            "---\n"
            "# Probe skill\n"
        )
    provider = ThreadingHTTPServer(("127.0.0.1", 0), FakeProvider)
    provider_thread = threading.Thread(target=provider.serve_forever, daemon=True)
    provider_thread.start()
    with open(os.path.join(scratch, ".jani", "settings.json"), "w", encoding="utf-8") as handle:
        json.dump(
            {
                "version": 1,
                "model": "probe",
                "models": {
                    "probe": {
                        "model": "probe-model",
                        "apiBase": f"http://127.0.0.1:{provider.server_port}/v1",
                        "apiKeyEnv": "JANI_API_KEY",
                    },
                    "alt": {
                        "model": "alt-model",
                        "apiBase": f"http://127.0.0.1:{provider.server_port}/v1",
                        "apiKeyEnv": "JANI_API_KEY",
                    },
                },
                "permissions": {
                    "defaultMode": "default",
                    "allow": ["Read(./**)"],
                    "ask": ["Bash(git push *)"],
                    "deny": ["Read(./.env)"],
                },
            },
            handle,
        )
    environment = os.environ.copy()
    environment.update(
        {
            "JANI_API_BASE": f"http://127.0.0.1:{provider.server_port}/v1",
            "JANI_API_KEY": "probe-secret-key",
            "JANI_MODEL": "probe-model",
        }
    )

    kernel32.FreeConsole()
    if not kernel32.AllocConsole():
        fail("AllocConsole")

    stdin_handle = kernel32.CreateFileW(
        "CONIN$",
        GENERIC_READ | GENERIC_WRITE,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        None,
        OPEN_EXISTING,
        0,
        None,
    )
    stdout_handle = kernel32.CreateFileW(
        "CONOUT$",
        GENERIC_READ | GENERIC_WRITE,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        None,
        OPEN_EXISTING,
        0,
        None,
    )
    if stdin_handle == wintypes.HANDLE(-1).value:
        fail("open CONIN$")
    if stdout_handle == wintypes.HANDLE(-1).value:
        fail("open CONOUT$")

    width, height = 120, 40
    kernel32.SetConsoleScreenBufferSize(stdout_handle, COORD(width, height))
    window = SMALL_RECT(0, 0, width - 1, height - 1)
    kernel32.SetConsoleWindowInfo(stdout_handle, True, ctypes.byref(window))

    child_in = duplicate(stdin_handle)
    child_out = duplicate(stdout_handle)
    in_fd = msvcrt.open_osfhandle(child_in.value, os.O_RDONLY)
    out_fd = msvcrt.open_osfhandle(child_out.value, os.O_WRONLY)

    process = subprocess.Popen(
        [binary],
        cwd=scratch,
        stdin=in_fd,
        stdout=out_fd,
        stderr=out_fd,
        close_fds=False,
        env=environment,
    )

    result = 1
    try:
        welcome = stable_screen(width, height, "ctrl+c")
        print("--- welcome screen ---")
        print(welcome)
        print("--- markers ---")
        print("welcome_logo:", "GOVERNED ENGINEERING AGENT" in welcome)
        print("welcome_prompt:", "Type a message" in welcome)
        print("welcome_hints:", "ctrl+c" in welcome)
        print("env_model_applied:", "probe-model" in welcome)

        # F1 must only draft text; no provider or tool request is allowed. The
        # first injected record can race crossterm's input reader, and the
        # shortcut only fires from an empty editor, so clear and retry once.
        for _ in range(2):
            key(stdin_handle, VK_ESCAPE)
            time.sleep(0.3)
            key(stdin_handle, VK_F1)
            drafted = stable_screen(
                width, height, "Map modules and dependencies.", timeout=8.0
            )
            if "Map modules and dependencies." in drafted:
                break
        starter_drafted = "Map modules and dependencies." in drafted
        starter_no_request = len(FakeProvider.captured) == 0
        print("starter_drafted:", starter_drafted)
        print("starter_no_request:", starter_no_request)
        key(stdin_handle, VK_ESCAPE)
        cleared = stable_screen(width, height, "Type a message", timeout=10.0)
        starter_cleared = "Map modules and dependencies." not in cleared
        print("starter_cleared:", starter_cleared)

        # Shift+Enter inserts a line break but must not submit any model call.
        type_text(stdin_handle, "first line")
        key(stdin_handle, VK_RETURN, "\r", SHIFT_PRESSED)
        type_text(stdin_handle, "second line")
        multiline = stable_screen(width, height, "second line", timeout=10.0)
        multiline_visible = "first line" in multiline and "second line" in multiline
        multiline_did_not_submit = len(FakeProvider.captured) == 0
        print("multiline_visible:", multiline_visible)
        print("multiline_did_not_submit:", multiline_did_not_submit)
        key(stdin_handle, VK_ESCAPE)

        type_text(stdin_handle, "/status\r")
        status = stable_screen(width, height, "Task:")
        print("--- status screen ---")
        print(status)
        print("status_ran:", "Task:" in status)
        print("status_agent_label:", "jani" in status)
        print("no_usage_dump:", "USAGE:" not in status)
        permissions_loaded = "Project permissions: mode=default allow=1 ask=1 deny=1" in status
        print("permissions_loaded:", permissions_loaded)

        # /skills must render a structured catalog block, not one dim paragraph:
        # an accent header, a marked skill name, the description and a readable
        # path. It must not call the provider.
        type_text(stdin_handle, "/skills\r")
        skills_screen = stable_screen(width, height, "◆ probe-skill", timeout=10.0)
        print("--- skills screen ---")
        print(skills_screen)
        skills_header = "skills · " in skills_screen and "discovered" in skills_screen
        skill_paths = [line for line in skills_screen.splitlines() if "↳" in line]
        skills_block = (
            skills_header
            and "◆ probe-skill" in skills_screen
            and "Probe the styled skills listing" in skills_screen
            and any("SKILL.md" in line and "\\\\?\\" not in line for line in skill_paths)
        )
        skills_no_request = len(FakeProvider.captured) == 0
        print("skills_header_rendered:", skills_header)
        print("skills_block_rendered:", skills_block)
        print("skills_no_request:", skills_no_request)

        type_text(stdin_handle, "hello\r")
        busy = wait_for(width, height, "step 1/", timeout=5.0)
        print("--- busy screen ---")
        print(busy)
        print("busy_live_activity:", "step 1/" in busy)
        tool = wait_for(width, height, "reading notes.txt", timeout=15.0)
        print("--- tool screen ---")
        print(tool)
        print("tool_activity_rendered:", "reading notes.txt" in tool)
        answer = wait_until_idle(width, height, "• beta item", timeout=25.0)
        print("--- answer screen ---")
        print(answer)
        print("answer_rendered:", "Hello from the fake provider." in answer)
        print("markdown_list_rendered:", "• alpha item" in answer and "• beta item" in answer)
        table_rendered = (
            "#  │  claim  │  verdict" in answer
            and "1  │  md  │  ok" in answer
            and "---" not in answer
        )
        print("markdown_table_rendered:", table_rendered)
        print("no_raw_markdown:", "**" not in answer and "`" not in answer)
        print(
            "reasoning_folded:",
            "reasoning line" in answer and "checking the fake provider" not in answer,
        )
        print("turn_counted:", "1 turn " in answer or "1 turn" in answer)
        tool_rows = [line for line in answer.splitlines() if "reading notes.txt" in line]
        tools_collapsed = (
            "▸ tools · 5 calls" in answer
            and "last 3 shown" in answer
            and len(tool_rows) == 3
            and any("ms" in line for line in tool_rows)
        )
        print("--- collapsed tools ---")
        print(answer)
        print("tool_calls_collapsed_to_three:", tools_collapsed)
        print("tool_count_readable:", "tools: 5 ok" in answer)
        tools_row = row_of(answer, "▸ tools · 5 calls")
        tools_expanded = False
        tools_expand_shows_all = False
        tools_recollapsed = False
        if tools_row >= 0:
            click(stdin_handle, 5, tools_row)
            expanded_tools = wait_for(width, height, "▾ tools · 5 calls", timeout=5.0)
            expanded_rows = [
                line for line in expanded_tools.splitlines() if "reading notes.txt" in line
            ]
            tools_expanded = "▾ tools · 5 calls" in expanded_tools
            tools_expand_shows_all = len(expanded_rows) == 5
            print("--- expanded tools ---")
            print(expanded_tools)
            print("tool_calls_expand_to_five:", tools_expanded and tools_expand_shows_all)
            collapsed_row = row_of(expanded_tools, "▾ tools · 5 calls")
            if collapsed_row >= 0:
                click(stdin_handle, 5, collapsed_row)
                recollapsed = wait_for(width, height, "▸ tools · 5 calls", timeout=5.0)
                tools_recollapsed = (
                    "▸ tools · 5 calls" in recollapsed
                    and len(
                        [
                            line
                            for line in recollapsed.splitlines()
                            if "reading notes.txt" in line
                        ]
                    )
                    == 3
                )
        print("tool_calls_recollapse:", tools_recollapsed)
        requests = [json.loads(body) for body in FakeProvider.captured]
        print("provider_requests:", len(FakeProvider.captured))
        print("five_tool_results_round_trip:", len(requests) == 2 and FakeProvider.captured[1].count("call_probe_") >= 5)
        print(
            "streaming_requested:",
            bool(requests) and all(request.get("stream") is True for request in requests),
        )
        print(
            "key_not_in_body:",
            all("probe-secret-key" not in body for body in FakeProvider.captured),
        )

        # Scrolling: /help lengthens the transcript past the viewport. PageUp,
        # the wheel and a click on a fold label each have to move the view.
        type_text(stdin_handle, "/help\r")
        overflow = stable_screen(width, height, "Any other input", timeout=10.0)
        print("--- overflow screen ---")
        print(overflow)
        print("scrollbar_rendered:", "║" in overflow or "█" in overflow)
        print("following_at_bottom:", "• beta item" in overflow)
        print("earlier_block_hidden:", jani_version not in overflow)

        key(stdin_handle, VK_PAGE_UP)
        keyed = stable_screen(width, height, "below the bottom", timeout=10.0)
        print("--- page up ---")
        print(keyed)
        print("pageup_scrolls:", "below the bottom" in keyed)

        enable_mouse(stdin_handle)
        for _ in range(8):
            wheel(stdin_handle, 40, 10, 120)
            time.sleep(0.05)
        # The app drains one record per animation tick, so wait for the effect of
        # the whole burst instead of settling on the first frame that matches.
        wait_for(width, height, "▸ think", timeout=10.0)
        scrolled = stable_screen(width, height, jani_version, timeout=10.0)
        print("--- wheel up ---")
        print(scrolled)
        print("wheel_scroll_up:", "below the bottom" in scrolled and scrolled != keyed)
        earlier = jani_version in scrolled and "Any other input" not in scrolled
        print("earlier_content_visible:", earlier)
        fold_row = row_of(scrolled, "▸ think")
        if fold_row < 0:
            # The styled /skills block makes the transcript taller than the top
            # window; scroll back down just enough to expose the fold label.
            for _ in range(6):
                wheel(stdin_handle, 40, 10, -120)
                time.sleep(0.05)
            fold_screen = stable_screen(width, height, "▸ think", timeout=10.0)
            fold_row = row_of(fold_screen, "▸ think")
        print("fold_row:", fold_row)
        click_expanded = False
        if fold_row >= 0:
            click(stdin_handle, 5, fold_row)
            expanded = wait_for(width, height, "checking the fake provider", timeout=5.0)
            click_expanded = "checking the fake provider" in expanded
            print("--- click expand ---")
            print(expanded)
        print("click_expands_thinking:", click_expanded)
        for _ in range(12):
            wheel(stdin_handle, 40, 10, -120)
            time.sleep(0.05)
        resumed = stable_screen(width, height, "Any other input", timeout=10.0)
        print("--- wheel down ---")
        print(resumed)
        print("wheel_resume_follow:", "below the bottom" not in resumed)
        print("back_at_bottom:", "Any other input" in resumed and jani_version not in resumed)

        # Slash palette: "/" opens a modal, the query filters it, the arrow keys
        # select, and Enter runs the highlighted command end to end.
        type_text(stdin_handle, "/")
        palette = stable_screen(width, height, "enter run", timeout=10.0)
        print("--- command palette ---")
        print(palette)
        body = palette_body(palette)
        palette_open = (
            "commands" in body
            and "/status" in body
            and "Show frozen authority" in body
        )
        print("palette_opens:", palette_open)

        type_text(stdin_handle, "st")
        filtered = stable_screen(width, height, "commands · st", timeout=10.0)
        print("--- command palette filter ---")
        print(filtered)
        filtered_body = palette_body(filtered)
        palette_filter = (
            "commands · st" in filtered_body
            and "/status" in filtered_body
            and "/budget" not in filtered_body
        )
        print("palette_filters:", palette_filter)

        key(stdin_handle, VK_ESCAPE, "\x1b")
        closed = stable_screen(width, height, "Any other input", timeout=10.0)
        print("--- palette closed ---")
        print(closed)
        palette_escape = "enter run" not in closed
        print("palette_escape_closes:", palette_escape)

        type_text(stdin_handle, "/")
        stable_screen(width, height, "enter run", timeout=10.0)
        key(stdin_handle, VK_DOWN)
        type_text(stdin_handle, "\r")
        executed = wait_until_idle(
            width, height, "Project permissions: mode=default", timeout=15.0
        )
        print("--- palette ran status ---")
        print(executed)
        palette_ran = (
            "Project permissions: mode=default" in executed
            and "enter run" not in executed
        )
        print("palette_runs_selected:", palette_ran)

        type_text(stdin_handle, "/new\r")
        cleared = stable_screen(width, height, "Conversation cleared", timeout=10.0)
        print("--- palette ran new ---")
        print(cleared)
        palette_new = (
            "Conversation cleared" in cleared
            and "Project permissions:" not in cleared
            and "enter run" not in cleared
        )
        print("palette_runs_new:", palette_new)

        # Model picker: "/model " opens the settings profiles, the query filters
        # them, Enter switches the live session, and a raw model ID is accepted
        # when no profile matches.
        type_text(stdin_handle, "/model ")
        picker = stable_screen(width, height, "enter switch", timeout=10.0)
        print("--- model picker ---")
        print(picker)
        picker_body = palette_body(picker, "model", "enter switch")
        model_picker_open = (
            "probe-model" in picker_body
            and "alt-model" in picker_body
            and "current" in picker_body
            and "enter switch" in picker_body
        )
        print("model_picker_opens:", model_picker_open)

        type_text(stdin_handle, "al")
        picker_filtered = stable_screen(width, height, "model · al", timeout=10.0)
        print("--- model picker filter ---")
        print(picker_filtered)
        filtered_picker_body = palette_body(
            picker_filtered, "model · al", "enter switch"
        )
        model_picker_filter = (
            "alt-model" in filtered_picker_body
            and "probe-model" not in filtered_picker_body
        )
        print("model_picker_filters:", model_picker_filter)

        type_text(stdin_handle, "\r")
        switched = wait_until_idle(
            width, height, "Model switched to alt-model", timeout=15.0
        )
        print("--- model switched ---")
        print(switched)
        model_switched = "Model switched to alt-model" in switched
        print("model_switch_end_to_end:", model_switched)

        type_text(stdin_handle, "/status\r")
        model_status = stable_screen(width, height, "Model: alt-model", timeout=10.0)
        model_status_ok = "Model: alt-model (profile alt)" in model_status
        print("model_status_reports_profile:", model_status_ok)

        type_text(stdin_handle, "/model ")
        stable_screen(width, height, "enter switch", timeout=10.0)
        type_text(stdin_handle, "custom-probe-model")
        custom_picker = stable_screen(
            width, height, "model · custom-probe-model", timeout=10.0
        )
        print("--- custom model row ---")
        print(custom_picker)
        custom_body = palette_body(
            custom_picker, "model · custom-probe-model", "enter switch"
        )
        custom_row = (
            "custom-probe-model" in custom_body
            and "use as a model ID" in custom_body
        )
        print("custom_model_row:", custom_row)

        type_text(stdin_handle, "\r")
        custom_switched = wait_until_idle(
            width, height, "Model switched to custom-probe-model", timeout=15.0
        )
        custom_model = "Model switched to custom-probe-model" in custom_switched
        print("custom_model_switch:", custom_model)

        type_text(stdin_handle, "/status\r")
        custom_status = stable_screen(
            width, height, "Model: custom-probe-model", timeout=10.0
        )
        custom_status_ok = (
            "Model: custom-probe-model" in custom_status
            and "Model: custom-probe-model (" not in custom_status
        )
        print("custom_model_has_no_profile:", custom_status_ok)

        type_text(stdin_handle, "/exit\r")
        try:
            code = process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            print("exit: timeout")
            code = -1

        print(f"exit={code}")
        ok = (
            "GOVERNED ENGINEERING AGENT" in welcome
            and "Type a message" in welcome
            and "ctrl+c" in welcome
            and "probe-model" in welcome
            and starter_drafted
            and starter_no_request
            and starter_cleared
            and multiline_visible
            and multiline_did_not_submit
            and "Task:" in status
            and "USAGE:" not in status
            and permissions_loaded
            and skills_header
            and skills_block
            and skills_no_request
            and "step 1/" in busy
            and "reading notes.txt" in tool
            and "Hello from the fake provider." in answer
            and "• alpha item" in answer
            and "• beta item" in answer
            and table_rendered
            and "**" not in answer
            and "`" not in answer
            and "reasoning line" in answer
            and "checking the fake provider" not in answer
            and "1 turn" in answer
            and tools_collapsed
            and "tools: 5 ok" in answer
            and tools_expanded
            and tools_expand_shows_all
            and tools_recollapsed
            and len(requests) == 2
            and FakeProvider.captured[1].count("call_probe_") >= 5
            and ("║" in overflow or "█" in overflow)
            and "• beta item" in overflow
            and jani_version not in overflow
            and "below the bottom" in keyed
            and "below the bottom" in scrolled
            and scrolled != keyed
            and jani_version in scrolled
            and "Any other input" not in scrolled
            and click_expanded
            and "below the bottom" not in resumed
            and "Any other input" in resumed
            and jani_version not in resumed
            and palette_open
            and palette_filter
            and palette_escape
            and palette_ran
            and palette_new
            and model_picker_open
            and model_picker_filter
            and model_switched
            and model_status_ok
            and custom_row
            and custom_model
            and custom_status_ok
            and len(FakeProvider.captured) == 2
            and bool(requests)
            and all(request.get("stream") is True for request in requests)
            and all("probe-secret-key" not in body for body in FakeProvider.captured)
            and code == 0
        )
        print("PROBE_RESULT=" + ("PASS" if ok else "FAIL"))
        result = 0 if ok else 1
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        provider.shutdown()
        provider.server_close()
        kernel32.FreeConsole()
        shutil.rmtree(scratch, ignore_errors=True)
        os.close(in_fd)
        os.close(out_fd)
    return result


if __name__ == "__main__":
    sys.exit(main())
