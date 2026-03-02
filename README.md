# juan-log.nvim
![Juan](https://static.wikia.nocookie.net/mamarre-estudios-espanol/images/a/a3/FB_IMG_1596591789564.jpg/revision/latest?cb=20200806023457&path-prefix=es)


A high-performance log viewer for Neovim, powered by Rust and Piece Tables.
It can open and search through 50GB+ files instantly (well, not yet but maybe in the future) without crashing Neovim.

## Should you use it?
If you regularly open logs, database dumps, or CSVs larger than 100MB and Neovim freezes, crashes, or eats all your RAM, yes. If you only deal with small files, standard Neovim is already perfectly fine.

## What does this plugin use?
- **Rust & C ABI:** The core engine is written in Rust and exposed to Neovim via LuaJIT FFI.
- **Memory Mapping (mmap):** Reads files directly from disk without loading them into RAM.
- **Rayon:** Parallel processing to count lines and index chunks instantly.
- **Piece Tables:** The same data structure used by VS Code to handle edits efficiently on massive documents.
- **Virtual Scrolling:** Lua dynamically fetches and renders only the visible lines in the Neovim buffer.

## Requirements
- Neovim >= 0.9.0
- Rust / Cargo (to compile the core engine)

## Installation

Using **lazy.nvim**:

```lua
{
    "minigian/juan-logs.nvim",
    build = "cargo build --release",
    ft = { "log", "txt", "csv", "json" }, -- load plugin for these filetypes
    config = function()
        require("juan_log").setup({
            threshold_size = 1024 * 1024 * 100, -- 100MB
            mode = "dynamic",
            lazy = true, -- background indexing. prevents neovim from freezing on 50GB files
            patterns = { "*.log", "*.txt", "*.csv", "*.json" }, -- MUST match the filetypes above to hijack the read
            enable_custom_statuscol = true, -- fakes absolute line numbers
            syntax = false -- set to true to enable native vim syntax (can be slow on huge files)
        })
    end
}
```

## Usage

When a file exceeds the `threshold_size`, it opens in dynamic mode. Since only a small chunk of the file is loaded in RAM, standard Vim search and navigation won't work across the entire file. Use the following instead:

### Commands
- `:Logfind <query>` - Search for a string across the entire file.
- `:LogLines` - Print the total number of lines in the file.
- `:LogJump <line>` - Teleport to an absolute line number.

### Keymaps (Normal Mode)
- `n` / `N` - Jump to the next/previous search match.
- `gg` - Jump to the absolute start of the file.
- `G` - Jump to the absolute end of the file.
