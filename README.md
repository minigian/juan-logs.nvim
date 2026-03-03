# juan-logs.nvim
![Juan](https://static.wikia.nocookie.net/mamarre-estudios-espanol/images/a/a3/FB_IMG_1596591789564.jpg/revision/latest?cb=20200806023457&path-prefix=es)

## What is this?
A high-performance log viewer for Neovim, powered by Rust and Piece Tables.
This plugin lets you open large text files (gigabytes) slightly faster than vanilla Neovim without crashing it. It allows you to use Neovim (including plugins) smoothly, with minimal RAM and CPU usage while opening large files.

![Proof](https://github.com/user-attachments/assets/815e1772-1016-4223-ac04-c3cc0003b9b5)

## Should you use it?
If you regularly open logs, database dumps, or CSVs larger than 100MB and Neovim freezes, crashes, or eats all your RAM, yes. If you only deal with small files, standard Neovim is already perfectly fine.

## What does this plugin use?
- **Rust & C ABI:** The core engine is written in Rust and exposed to Neovim via LuaJIT FFI.
- **Memory Mapping (mmap):** Reads files directly from disk without loading them into RAM.
- **Rayon:** Parallel processing to count lines and index chunks instantly.
- **Piece Tables:** The same data structure used by VS Code to handle edits efficiently on massive documents without shifting gigabytes of memory.

## FAQ

**Q: How do you open a 50GB file without blowing up my RAM?**  
A: We don't read the file. We `mmap` it and let the OS deal with the paging nightmare. To the engine, the file is just one massive read-only byte slice. 

**Q: How do you know where line 5,000,000 is without reading everything?**  
A: We spawn a background worker that blasts through the file in 5MB chunks using SIMD (`memchr`) to count newlines. It builds a sparse index of byte offsets. Until the index finishes, the bottom of the file is just the abyss.

**Q: What if I get bored and close the buffer while it's still indexing?**  
A: We flip an atomic `cancel_token`. The background thread sees the cyanide pill, stops parsing, and dies quietly so it doesn't keep eating your CPU in the background. 

**Q: How do edits work if the file is read-only memory mapped?**  
A: A classic Piece Table. Deleting a million lines just drops a node from a `Vec`. New text gets dumped into a heap-allocated memory buffer. When you save, we stitch it all together and atomic-swap the file.

**Q: Why the FFI boundary?**  
A: RPC overhead is slow. JSON serialization is slow. We use `extern "C"` to hand out raw memory pointers across the boundary directly to LuaJIT. Lua reads the C strings and renders the UI. It's standard `unsafe` boilerplate, but it's fast.

**Q: Why not use Less or Vimpager?**

A: Because this plugin lets you edit and use your Neovim keymaps natively through FFI; it's not just a viewer.

**Q: Why not bigfile.nvim or faster.nvim?**

A: Existing plugins just disable syntax highlighting and plugins to save CPU, but they still load the entire file into Neovim's RAM buffer. If you open a 10GB file, Neovim will still crash. JuanLog bypasses Neovim's buffer entirely, using a Rust piece-table and mmap to stream only the visible lines. It uses virtually 0 extra RAM, no matter if the file is 100MB or 50GB.

## Requirements
- Neovim >= 0.9.0
- Rust / Cargo (to compile the core engine)

## Installation

Using **lazy.nvim**:

```lua
{
    "minigian/juan-logs.nvim",
    build = "cargo build --release",
    config = function()
        require("juanlog").setup({
            threshold_size = 1024 * 1024 * 100, -- 100MB
            mode = "dynamic",
            lazy = true, -- background indexing. prevents neovim from freezing on 50GB files
            patterns = { "*.log", "*.txt", "*.csv", "*.json" }, -- Use the plugin for these filetypes
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

## Important

Consider this a proof of concept. This software does NOT perform magic; opening speed depends entirely on your hardware. Be careful what you do, friend.

Feel free to report any issue.

I'm tired...





