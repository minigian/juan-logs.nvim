local uv = vim.loop or vim.uv

local function get_os_info()
    local sysname = uv.os_uname().sysname
    if sysname == "Windows_NT" then
        return "windows", "dll"
    elseif sysname == "Darwin" then
        return "macos", "dylib"
    else
        return "linux", "so"
    end
end

local function build()
    local os_name, ext = get_os_info()
    local repo = "minigian/juan-logs.nvim"
    
    local release_file = string.format("libjuanlog-%s.%s", os_name, ext)
    local download_url = string.format("https://github.com/%s/releases/latest/download/%s", repo, release_file)

    local script_path = debug.getinfo(1, "S").source:sub(2)
    local plugin_root = vim.fn.fnamemodify(script_path, ":h")
    local lib_dir = plugin_root .. "/lib"

    if vim.fn.isdirectory(lib_dir) == 0 then
        vim.fn.mkdir(lib_dir, "p")
    end

    local out_file = string.format("%s/libjuanlog.%s", lib_dir, ext)

    print("[JuanLog] Fetching pre-built binary for " .. os_name .. "...")

    local cmd
    if vim.fn.executable("curl") == 1 then
        cmd = { "curl", "-fsL", "-o", out_file, download_url }
    elseif vim.fn.executable("wget") == 1 then
        cmd = { "wget", "-qO", out_file, download_url }
    elseif os_name == "windows" then
        cmd = { "powershell", "-Command", string.format("$ErrorActionPreference = 'Stop'; Invoke-WebRequest -Uri '%s' -OutFile '%s'", download_url, out_file) }
    else
        print("[JuanLog] Error: Need 'curl' or 'wget' to download.")
        return
    end

    local result = vim.fn.system(cmd)
    
    if vim.v.shell_error ~= 0 then
        print("[JuanLog] Download failed. Falling back to cargo build...")
        if vim.fn.executable("cargo") == 1 then
            local build_cmd = string.format('cd "%s" && cargo build --release', plugin_root)
            if os_name == "windows" then
                build_cmd = string.format('cd /d "%s" && cargo build --release', plugin_root)
            end
            
            vim.fn.system(build_cmd)
            
            local cargo_output_name = "libjuanlog." .. ext
            if os_name == "windows" then
                cargo_output_name = "juanlog.dll"
            end
            
            local target_bin = string.format("%s/target/release/%s", plugin_root, cargo_output_name)
            if vim.fn.filereadable(target_bin) == 1 then
                uv.fs_copyfile(target_bin, out_file)
                print("[JuanLog] Local build finished.")
            else
                print("[JuanLog] Error: Cargo build failed.")
            end
        else
            print("[JuanLog] Error: Download failed and 'cargo' is missing.")
        end
    else
        print("[JuanLog] Binary downloaded to " .. out_file)
    end
end

build()