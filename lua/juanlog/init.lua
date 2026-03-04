local config = require("juanlog.config")
local handlers = require("juanlog.handlers")

local M = {}

function M.setup(user_config)
    config.setup(user_config)

    vim.api.nvim_create_autocmd("BufReadCmd", {
        pattern = config.patterns,
        callback = function(ev)
            local file = vim.fn.expand("<amatch>:p") -- absolute path
            local stat = vim.loop.fs_stat(file)

            if not stat or stat.type == "directory" then
                return
            end

            -- hijack huge files, pass small ones to standard vim
            if stat.size > config.threshold_size then
                vim.schedule(function()
                    if vim.api.nvim_buf_is_valid(ev.buf) then
                        handlers.attach_to_buffer(ev.buf, file)
                    end
                end)
            else
                vim.schedule(function()
                    if not vim.api.nvim_buf_is_valid(ev.buf) then return end
                    local bufnr = ev.buf
                    vim.api.nvim_exec_autocmds("BufReadPre", { buffer = bufnr })

                    vim.api.nvim_buf_call(bufnr, function()
                        local was_modifiable = vim.bo[bufnr].modifiable
                        vim.bo[bufnr].modifiable = true

                        -- fallback: just read it normally (we're in BufReadCmd so default read was skipped)
                        vim.cmd('silent! read ' .. vim.fn.fnameescape(file))
                        vim.cmd('1delete _')

                        vim.bo[bufnr].modified = false
                        vim.bo[bufnr].modifiable = was_modifiable
                    end)

                    vim.api.nvim_exec_autocmds("BufReadPost", { buffer = bufnr })
                end)
            end
        end
    })
end

return M
