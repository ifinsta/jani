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
VK_PAGE_UP = 0x21

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
    buffer = ctypes.create_unicode_buffer(width * height)
    read = wintypes.DWORD()
    if not kernel32.ReadConsoleOutputCharacterW(
        handle, buffer, width * height, COORD(0, 0), ctypes.byref(read)
    ):
        kernel32.CloseHandle(handle)
        fail("ReadConsoleOutputCharacterW")
    kernel32.CloseHandle(handle)
    text = buffer[: read.value]
    return "\n".join(text[row * width : (row + 1) * width].rstrip() for row in range(height))


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
                                        "index": 0,
                                        "id": "call_probe",
                                        "type": "function",
                                        "function": {
                                            "name": "workspace_read",
                                            "arguments": '{"path":"notes.txt"}',
                                        },
                                    }
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
    scratch = os.path.abspath(sys.argv[2])
    shutil.rmtree(scratch, ignore_errors=True)
    os.makedirs(os.path.join(scratch, ".jani"))
    with open(os.path.join(scratch, "notes.txt"), "w", encoding="utf-8") as handle:
        handle.write("probe notes\n")
    with open(os.path.join(scratch, ".jani", "settings.json"), "w", encoding="utf-8") as handle:
        json.dump(
            {
                "version": 1,
                "permissions": {
                    "defaultMode": "default",
                    "allow": ["Read(./**)"],
                    "ask": ["Bash(git push *)"],
                    "deny": ["Read(./.env)"],
                },
            },
            handle,
        )
    provider = ThreadingHTTPServer(("127.0.0.1", 0), FakeProvider)
    provider_thread = threading.Thread(target=provider.serve_forever, daemon=True)
    provider_thread.start()
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
        print("welcome_logo:", "GOVERNED CODING AGENT" in welcome)
        print("welcome_prompt:", "Type a message" in welcome)
        print("welcome_hints:", "ctrl+c" in welcome)
        print("env_model_applied:", "probe-model" in welcome)

        type_text(stdin_handle, "/status\r")
        status = stable_screen(width, height, "Task:")
        print("--- status screen ---")
        print(status)
        print("status_ran:", "Task:" in status)
        print("status_agent_label:", "jani" in status)
        print("no_usage_dump:", "USAGE:" not in status)
        permissions_loaded = "Project permissions: mode=default allow=1 ask=1 deny=1" in status
        print("permissions_loaded:", permissions_loaded)

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
        print("no_raw_markdown:", "**" not in answer and "`" not in answer)
        print(
            "reasoning_folded:",
            "reasoning line" in answer and "checking the fake provider" not in answer,
        )
        print("turn_counted:", "1 turn " in answer or "1 turn" in answer)
        requests = [json.loads(body) for body in FakeProvider.captured]
        print("provider_requests:", len(FakeProvider.captured))
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
        print("earlier_block_hidden:", "Jani v0.1.0" not in overflow)

        key(stdin_handle, VK_PAGE_UP)
        keyed = stable_screen(width, height, "below the bottom", timeout=10.0)
        print("--- page up ---")
        print(keyed)
        print("pageup_scrolls:", "below the bottom" in keyed)

        enable_mouse(stdin_handle)
        for _ in range(8):
            wheel(stdin_handle, 40, 10, 120)
            time.sleep(0.05)
        scrolled = stable_screen(width, height, "Jani v0.1.0", timeout=10.0)
        print("--- wheel up ---")
        print(scrolled)
        print("wheel_scroll_up:", "below the bottom" in scrolled and scrolled != keyed)
        earlier = "Jani v0.1.0" in scrolled and "Any other input" not in scrolled
        print("earlier_content_visible:", earlier)
        fold_row = row_of(scrolled, "▸ think")
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
        print("back_at_bottom:", "Any other input" in resumed and "Jani v0.1.0" not in resumed)

        type_text(stdin_handle, "/exit\r")
        try:
            code = process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            print("exit: timeout")
            code = -1

        print(f"exit={code}")
        ok = (
            "GOVERNED CODING AGENT" in welcome
            and "Type a message" in welcome
            and "ctrl+c" in welcome
            and "probe-model" in welcome
            and "Task:" in status
            and "USAGE:" not in status
            and permissions_loaded
            and "step 1/" in busy
            and "reading notes.txt" in tool
            and "Hello from the fake provider." in answer
            and "• alpha item" in answer
            and "• beta item" in answer
            and "**" not in answer
            and "`" not in answer
            and "reasoning line" in answer
            and "checking the fake provider" not in answer
            and "1 turn" in answer
            and ("║" in overflow or "█" in overflow)
            and "• beta item" in overflow
            and "Jani v0.1.0" not in overflow
            and "below the bottom" in keyed
            and "below the bottom" in scrolled
            and scrolled != keyed
            and "Jani v0.1.0" in scrolled
            and "Any other input" not in scrolled
            and click_expanded
            and "below the bottom" not in resumed
            and "Any other input" in resumed
            and "Jani v0.1.0" not in resumed
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
