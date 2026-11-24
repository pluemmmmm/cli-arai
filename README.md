# cli arai (Rust + Rune AI Desktop App & CLI)

โปรแกรม **Desktop App + CLI** ที่สร้างด้วย **Rust** + **egui/eframe** + **Rune** (embeddable scripting language) พอสั่ง `cargo run` จะเปิดหน้าต่างโปรแกรม Desktop ขึ้นมาทันที พร้อมระบบตรวจจับ AI ในเครื่อง, หน้าต่างแสดงการแก้ไขไฟล์ (Changes/Diff UI), และ Terminal แยกในหน้าต่างเดียว

## ฟีเจอร์หลัก
1. **Native Desktop App (`src/desktop.rs`)** ด้วย `eframe` / `egui` (รองรับฟอนต์ภาษาไทยอัตโนมัติบน macOS):
   - **แถบด้านบน**: Dropdown เลือก AI Provider ที่ตรวจพบในเครื่อง, ปุ่ม Refresh AI, ปุ่ม Refresh Diff, และปุ่มซ่อน/แสดง Terminal
   - **ตรงกลาง (AI & Rune Console)**: ช่องกรอก Prompt ส่งคุยกับ AI หรือกด **Run Rune** เพื่อรันโค้ด Rune แบบ `async/await` พร้อมปุ่มลัดรัน `examples/hello.rn`
   - **ด้านขวา (Changes / Diff Viewer)**: แสดงรายการไฟล์ที่ถูกแก้ไขพร้อม Diff สีเขียว (`+`) / แดง (`-`) แบบ real-time จากทั้ง `git` และการเขียนไฟล์ผ่าน Rune
   - **ด้านล่าง (Separate Terminal)**: หน้าต่าง Terminal แยกอิสระ (ย่อ/ขยายได้) สำหรับพิมพ์คำสั่ง shell เช่น `ls`, `git status`, `cargo test`
2. **AI Auto-Detection (`src/ai.rs`)** — ตรวจสอบ AI ในเครื่องอัตโนมัติ (API keys, AI CLIs เช่น `agy`, `claude`, `codex`, `gemini`, `ollama`, และ local Ollama server)
3. **Rune Scripting Engine (`src/rune_engine.rs`)** — รองรับ `async/await` + `tokio` พร้อม Custom Rust Module `cli::*` (`read_file`, `write_file`, `list_dir`, `sleep_ms`, `run_cmd`, `detect_ai`, `ask_ai`)

## การติดตั้งและรัน

### macOS / Linux

> **หมายเหตุ**: หากเครื่องยังไม่มี Rust (`rustc` / `cargo`) ให้ติดตั้งก่อนด้วย:
> ```bash
> curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
> # หรือ (macOS)
> brew install rust
> ```

### Windows (PowerShell)

1. **ติดตั้ง Rust**:
   ```powershell
   winget install Rustlang.Rustup
   ```
   *(หรือดาวน์โหลดจาก https://rustup.rs/)*

2. **ติดตั้ง C/C++ Linker (เลือกอย่างใดอย่างหนึ่ง)**:
   - **แบบที่ 1 (แนะนำ — Visual Studio Build Tools, ~6–8 GB)**: ดาวน์โหลดจาก https://visualstudio.microsoft.com/visual-cpp-build-tools/ แล้วเลือก **Desktop development with C++**
   - **แบบที่ 2 (ขนาดเล็ก — MinGW, ~300 MB)**:
     ```powershell
     winget install BrechtSanders.WinLibs.POSIX.UCRT
     rustup default stable-x86_64-pc-windows-gnu
     ```

3. **หลังติดตั้งเสร็จ** ให้ปิดแล้วเปิด **PowerShell** ใหม่ (แนะนำให้รันบน PowerShell แทน Git Bash เพื่อไม่ให้ `link.exe` ชนกัน)

---

### คำสั่งรันโปรแกรม

```bash
# 1. เปิดโปรแกรม Desktop App (ค่าเริ่มต้น)
cargo run

# 2. หรือเปิดโหมด Terminal UI (TUI) ใน Terminal
cargo run -- tui

# 3. ตรวจสอบ AI ทั้งหมดที่มีในเครื่อง
cargo run -- ai list

# 4. ถาม AI แบบ one-shot
cargo run -- ai ask "อธิบาย ownership ใน Rust แบบสั้นๆ"

# 5. รันสคริปต์ Rune (.rn)
cargo run -- run examples/hello.rn

# 6. เปิดโหมด Rune REPL
cargo run -- repl
```

### รันแบบ Auto-reload ระหว่างพัฒนา (Hot Reload)

หากต้องการให้โปรแกรมรีสตาร์ทอัตโนมัติเมื่อแก้ไขไฟล์ใน `src` หรือ `Cargo.toml` สามารถใช้ `watchexec` ได้:

- **macOS / Linux**:
  ```bash
  brew install watchexec   # หรือ cargo install watchexec-cli
  watchexec -r -w src -w Cargo.toml -- cargo run
  ```

- **Windows (PowerShell)**:
  ```powershell
  winget install watchexec.watchexec
  # ปิดแล้วเปิด PowerShell ใหม่ จากนั้นรัน:
  watchexec -r -w src -w Cargo.toml --shell=none -- cargo run
  ```
