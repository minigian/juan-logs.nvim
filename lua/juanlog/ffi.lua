local ffi = require("ffi")

-- keep this in sync with the rust struct/externs or segfaults will happen.
ffi.cdef [[
    typedef struct LogEngine LogEngine;
    LogEngine* log_engine_new(const char* path, bool lazy);
    float log_engine_get_progress(const LogEngine* engine);
    size_t log_engine_total_lines(LogEngine* engine);
    bool log_engine_get_block(LogEngine* engine, size_t start_line, size_t num_lines, char* out_buffer, size_t max_len, size_t* out_len);
    bool log_engine_get_eof_block(LogEngine* engine, size_t num_lines, char* out_buffer, size_t max_len, size_t* out_len);
    void log_engine_apply_edit(LogEngine* engine, size_t start_line, size_t num_deleted, const char* new_text);
    bool log_engine_save(const LogEngine* engine, const char* path);
    bool log_engine_save_async(const LogEngine* engine, const char* path);
    float log_engine_get_save_progress(const LogEngine* engine);
    ptrdiff_t log_engine_search(LogEngine* engine, const char* query, size_t start_line);
    ptrdiff_t log_engine_search_backward(LogEngine* engine, const char* query, size_t start_line);
    void log_engine_free(LogEngine* engine);
]]

local function get_lib_path()
    local sysname = vim.loop.os_uname().sysname
    local ext = sysname == "Windows_NT" and "dll" or (sysname == "Darwin" and "dylib" or "so")
    local lib_name = "libjuanlog." .. ext

    -- check local dev path first
    local local_dev_path = vim.fn.stdpath("config") .. "/lua/juan_log/bin/" .. lib_name
    if vim.loop.fs_stat(local_dev_path) then
        return local_dev_path
    end

    local str = debug.getinfo(1, "S").source:sub(2)
    local plugin_root = str:match("(.*[/\\])"):gsub("lua[/\\]juanlog[/\\]$", "")

    local prebuilt_path = plugin_root .. "bin/" .. lib_name
    if vim.loop.fs_stat(prebuilt_path) then
        return prebuilt_path
    end

    -- fallback to release path
    return plugin_root .. "target/release/" .. lib_name
end

-- lazy load the rust binary. don't penalize startup time for files we don't care about.
local _lib_cache = nil
local function get_lib()
    if _lib_cache then return _lib_cache end
    
    local so_path = get_lib_path()
    local ok, lib = pcall(ffi.load, so_path)

    if not ok then
        vim.schedule(function()
            vim.notify("[JuanLog] Warning: Rust binary not found.\nPlugin is disabled.", vim.log.levels.WARN)
        end)
        return nil
    end
    
    _lib_cache = lib
    return _lib_cache
end

return {
    get_lib = get_lib
}