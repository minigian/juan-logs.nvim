local ffi = require("ffi")
local ffi_mod = require("juanlog.ffi")
local config = require("juanlog.config")

local M = {}

-- global state to map buffers to rust engines
_G.JuanLogStates = _G.JuanLogStates or {}

-- custom status column to fake absolute line numbers.
-- clean and fixed width. no more text jumping around.
_G._juan_log_statuscol = function()
    local winid = vim.g.statusline_winid or vim.api.nvim_get_current_win()
    local b = vim.api.nvim_win_get_buf(winid)
    local st = _G.JuanLogStates[b]
    
    if st and config.mode == "dynamic" then
        if not config.lazy and st.indexing_progress < 1.0 then
            return "%=~ "
        elseif st.is_eof_mode then
            return "%=~ "
        else
            return string.format("%%=%d ", st.offset + vim.v.lnum)
        end
    end
    return "%=%l "
end

function M.fetch_lines(engine, start, count)
    local lib = ffi_mod.get_lib()
    local len_ptr = ffi.new("size_t[1]")
    local max_len = 5 * 1024 * 1024
    local buffer = ffi.new("char[?]", max_len)
    
    local success = lib.log_engine_get_block(engine, start, count, buffer, max_len, len_ptr)
    
    if not success then return {} end
    
    local length = tonumber(len_ptr[0])
    if length == 0 then return {} end

    local raw_text = ffi.string(buffer, length)
    
    -- clean up trailing newlines from the block fetch
    if raw_text:sub(-1) == "\n" then raw_text = raw_text:sub(1, -2) end
    if raw_text:sub(-1) == "\r" then raw_text = raw_text:sub(1, -2) end
    
    return vim.split(raw_text, "\n", { plain = true })
end

function M.fetch_eof_lines(engine, count)
    local lib = ffi_mod.get_lib()
    local len_ptr = ffi.new("size_t[1]")
    local max_len = 5 * 1024 * 1024
    local buffer = ffi.new("char[?]", max_len)
    
    local success = lib.log_engine_get_eof_block(engine, count, buffer, max_len, len_ptr)
    
    if not success then return {} end
    
    local length = tonumber(len_ptr[0])
    if length == 0 then return {} end

    local raw_text = ffi.string(buffer, length)
    if raw_text:sub(-1) == "\n" then raw_text = raw_text:sub(1, -2) end
    if raw_text:sub(-1) == "\r" then raw_text = raw_text:sub(1, -2) end
    
    return vim.split(raw_text, "\n", { plain = true })
end

-- bypass the 'modifiable = false' lock because we actually need to show text.
function M.force_set_lines(bufnr, start_row, end_row, strict, lines)
    local was_modifiable = vim.api.nvim_buf_get_option(bufnr, 'modifiable')
    
    if not was_modifiable then
        vim.api.nvim_buf_set_option(bufnr, 'modifiable', true)
    end
    
    vim.api.nvim_buf_set_lines(bufnr, start_row, end_row, strict, lines)
    
    if not was_modifiable then
        vim.api.nvim_buf_set_option(bufnr, 'modifiable', false)
    end
end

-- swallow the red error if the window died or the buffer desynced.
function M.safe_set_cursor(winid, pos)
    pcall(vim.api.nvim_win_set_cursor, winid, pos)
end

function M.load_all_lines(bufnr, engine, total_lines)
    local chunk_size = 50000 
    local loaded = 0
    
    -- disable undo history or nvim RAM usage will skyrocket
    vim.api.nvim_buf_set_option(bufnr, 'undolevels', -1)
    
    -- async recursive loading so we don't freeze the UI
    local function load_next_chunk()
        if not vim.api.nvim_buf_is_valid(bufnr) then return end
        
        local to_fetch = math.min(chunk_size, total_lines - loaded)
        local lines = M.fetch_lines(engine, loaded, to_fetch)
        
        if #lines > 0 then
            M.force_set_lines(bufnr, -1, -1, false, lines)
        end
        
        loaded = loaded + to_fetch
        
        if loaded < total_lines then
            vim.defer_fn(load_next_chunk, 5) -- yield to neovim's event loop
        else
            vim.api.nvim_buf_set_option(bufnr, 'modified', false)
        end
    end
    
    load_next_chunk()
end

-- "teleport" the visible window to a new location in the huge file
function M.jump_to_line(bufnr, state, found_line)
    if state.indexing_progress < 1.0 and found_line >= state.total then
        vim.notify("[JuanLog] Target line is not indexed yet.", vim.log.levels.WARN)
        return
    end

    local half_chunk = math.floor(state.chunk_size / 2)
    local new_offset = math.max(0, found_line - half_chunk)

    -- state.total is a lie if we are still indexing. trust rust to clamp it.
    if state.indexing_progress >= 1.0 and new_offset + state.chunk_size > state.total then
        new_offset = math.max(0, state.total - state.chunk_size)
    end

    state.updating = true
    local was_modified = vim.api.nvim_buf_get_option(bufnr, 'modified')
    local new_lines = M.fetch_lines(state.engine, new_offset, state.chunk_size)
    
    -- replace the entire buffer content safely
    M.force_set_lines(bufnr, 0, -1, false, new_lines)

    local new_row = (found_line - new_offset) + 1
    new_row = math.max(1, math.min(new_row, #new_lines))
    
    M.safe_set_cursor(0, {new_row, 0})
    
    state.offset = new_offset
    state.is_eof_mode = false
    vim.api.nvim_buf_set_option(bufnr, 'modified', was_modified)
    state.updating = false
    
    vim.cmd("normal! zz")
end

function M.jump_to_eof(bufnr, state)
    state.updating = true
    local was_modified = vim.api.nvim_buf_get_option(bufnr, 'modified')
    
    if state.indexing_progress < 1.0 then
        -- the abyss stares back. we don't know the line numbers yet.
        state.is_eof_mode = true
        local new_lines = M.fetch_eof_lines(state.engine, state.chunk_size)
        M.force_set_lines(bufnr, 0, -1, false, new_lines)
        M.safe_set_cursor(0, {#new_lines, 0})
    else
        -- normal jump, we know where the end is
        state.is_eof_mode = false
        local target = math.max(0, state.total - 1)
        M.jump_to_line(bufnr, state, target)
        return
    end
    
    vim.api.nvim_buf_set_option(bufnr, 'modified', was_modified)
    state.updating = false
    vim.cmd("normal! zz")
end

return M