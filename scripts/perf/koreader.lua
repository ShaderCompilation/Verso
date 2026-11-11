-- Run from the installed KOReader directory with KO_HOME in an isolated folder.
-- No input-device writes: dispatch recognized taps through the regular UI path.
require("setupkoenv")
local ffi = require("ffi")
ffi.cdef[[void verso_bench_mark(const char *name);]]
local function mark(s) ffi.C.verso_bench_mark(s) end
local original_require = require
local installed = false
function require(name)
    local module = original_require(name)
    if name == "device" then
        -- The unchanged installed launcher is deliberately not used: Verso's
        -- launcher owns pause/restore for both apps in this experiment.
        module.isStartupScriptUpToDate = function() return true end
    elseif name == "ui/uimanager" and not installed then
        installed = true
        local run = module.run
        function module:run(...)
            self.run = run
            local Event = require("ui/event")
            local Geom = require("ui/geometry")
            local ReaderUI = require("apps/reader/readerui")
            if os.getenv("BENCH_STARTUP_ONLY") == "1" then
                self:scheduleIn(6, function() mark("startup_done"); self:quit() end)
                return run(self, ...)
            end
            local function tap(label, x, y)
                mark(label)
                self:handleInputEvent(Event:new("Gesture", {ges="tap", pos=Geom:new{x=x,y=y,w=0,h=0}}))
            end
            self:scheduleIn(5, function()
                mark("open_book")
                ReaderUI:showReader(assert(os.getenv("BENCH_BOOK")))
            end)
            self:scheduleIn(16, function()
                local ui = assert(ReaderUI.instance, "reader did not open")
                mark("seek_text")
                ui.rolling:onGotoPage(100)
                print("BENCH pages", ui.document:getPageCount(), "font", ui.font.font_face)
            end)
            for i=1,24 do
                self:scheduleIn(18 + i * 0.9, function() tap("turn_" .. i, 1100, 1000) end)
            end
            self:scheduleIn(43, function()
                mark("menu_open")
                ReaderUI.instance.menu:onShowMenu()
            end)
            self:scheduleIn(45, function() tap("menu_close", 600, 1400) end)
            self:scheduleIn(47, function() mark("idle_begin") end)
            self:scheduleIn(59, function()
                mark("idle_end")
                ReaderUI.instance:onClose()
                self:quit()
            end)
            return run(self, ...)
        end
    elseif name == "apps/reader/readerui" and not module._bench_wrapped then
        module._bench_wrapped = true
        local paint = module.paintTo
        function module:paintTo(...)
            mark("reader_paint")
            return paint(self, ...)
        end
    elseif name == "document/credocument" and not module._bench_wrapped then
        module._bench_wrapped = true
        local set_size = module.setFontSize
        function module:setFontSize(size)
            local px = tonumber(os.getenv("BENCH_FONT_PX")) or size
            print("BENCH engine_font_px", px)
            return set_size(self, px)
        end
    end
    return module
end
arg = {assert(os.getenv("BENCH_DIRECTORY"))}
dofile("reader.lua")
