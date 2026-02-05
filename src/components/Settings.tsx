import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from '@/components/ui/dialog';
import { Button } from '@/components/ui/button';

interface SettingsProps {
  isOpen: boolean;
  onClose: () => void;
}

const STORAGE_KEY = 'claude-sessions-hotkey';
const IGNORE_CPU_KEY = 'claude-sessions-ignore-cpu';
const DEFAULT_HOTKEY = 'Control+Space';

export function Settings({ isOpen, onClose }: SettingsProps) {
  const [hotkey, setHotkey] = useState(DEFAULT_HOTKEY);
  const [isRecording, setIsRecording] = useState(false);
  const [recordedKeys, setRecordedKeys] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [ignoreCpu, setIgnoreCpu] = useState(false);

  // Load saved settings on mount
  useEffect(() => {
    const savedHotkey = localStorage.getItem(STORAGE_KEY);
    if (savedHotkey) {
      setHotkey(savedHotkey);
    }

    // Load and sync ignore CPU setting
    const savedIgnoreCpu = localStorage.getItem(IGNORE_CPU_KEY);
    if (savedIgnoreCpu === 'true') {
      setIgnoreCpu(true);
      invoke('set_ignore_cpu_setting', { value: true }).catch(console.error);
    }
  }, []);

  // Register hotkey with backend
  const registerHotkey = useCallback(async (shortcut: string) => {
    try {
      await invoke('register_shortcut', { shortcut });
      setError(null);
      return true;
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      return false;
    }
  }, []);

  // Handle key recording
  useEffect(() => {
    if (!isRecording) return;

    const handleKeyDown = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();

      const keys: string[] = [];

      if (e.metaKey) keys.push('Command');
      if (e.ctrlKey) keys.push('Control');
      if (e.altKey) keys.push('Option');
      if (e.shiftKey) keys.push('Shift');

      // Add the actual key if it's not a modifier
      const key = e.key;
      if (!['Meta', 'Control', 'Alt', 'Shift'].includes(key)) {
        // Convert key to proper format
        let formattedKey = key;
        if (key === ' ') formattedKey = 'Space';
        else if (key.length === 1) formattedKey = key.toUpperCase();
        else if (key.startsWith('Arrow')) formattedKey = key;
        else if (key.startsWith('F') && key.length <= 3) formattedKey = key; // F1-F12

        keys.push(formattedKey);
      }

      setRecordedKeys(keys);
    };

    const handleKeyUp = (e: KeyboardEvent) => {
      e.preventDefault();

      if (recordedKeys.length > 0 && !['Meta', 'Control', 'Alt', 'Shift'].includes(e.key)) {
        // We have a complete shortcut
        const shortcut = recordedKeys.join('+');
        setHotkey(shortcut);
        setIsRecording(false);
        setRecordedKeys([]);
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    window.addEventListener('keyup', handleKeyUp);

    return () => {
      window.removeEventListener('keydown', handleKeyDown);
      window.removeEventListener('keyup', handleKeyUp);
    };
  }, [isRecording, recordedKeys]);

  const handleSave = async () => {
    const success = await registerHotkey(hotkey);
    if (success) {
      localStorage.setItem(STORAGE_KEY, hotkey);
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    }
  };

  const handleClear = async () => {
    try {
      await invoke('unregister_shortcut');
      setHotkey('');
      localStorage.removeItem(STORAGE_KEY);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  return (
    <Dialog open={isOpen} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-[320px] gap-6">
        <DialogHeader>
          <DialogTitle>Settings</DialogTitle>
        </DialogHeader>

        <div className="space-y-3">
          <label className="text-sm font-medium text-foreground">
            Global Hotkey
          </label>
          <div
            className={`flex items-center justify-center h-11 rounded-lg border cursor-pointer transition-colors ${
              isRecording
                ? 'border-foreground/50 bg-foreground/5'
                : 'border-border bg-muted/50 hover:border-muted-foreground/50'
            }`}
            onClick={() => setIsRecording(true)}
          >
            <span className="text-sm text-foreground">
              {isRecording ? (
                recordedKeys.length > 0 ? recordedKeys.join(' + ') : 'Press keys...'
              ) : (
                hotkey || 'Click to set hotkey'
              )}
            </span>
          </div>
          <p className="text-xs text-muted-foreground">
            Click and press your desired key combination
          </p>

          {error && (
            <div className="p-3 rounded-lg bg-destructive/10 border border-destructive/20 text-destructive text-sm">
              {error}
            </div>
          )}

          {saved && (
            <div className="p-3 rounded-lg bg-emerald-400/10 border border-emerald-400/20 text-emerald-400 text-sm">
              Hotkey saved
            </div>
          )}
        </div>

        <div className="space-y-3">
          <label className="text-sm font-medium text-foreground">
            Status Detection
          </label>
          <div
            className="flex items-center justify-between p-3 rounded-lg border border-border bg-muted/50 cursor-pointer hover:border-muted-foreground/50"
            onClick={async () => {
              const newValue = !ignoreCpu;
              setIgnoreCpu(newValue);
              try {
                await invoke('set_ignore_cpu_setting', { value: newValue });
                localStorage.setItem(IGNORE_CPU_KEY, String(newValue));
              } catch (err) {
                console.error('Failed to save ignore CPU setting:', err);
                setIgnoreCpu(!newValue); // Revert on error
              }
            }}
          >
            <span className="text-sm text-foreground">Ignore CPU usage</span>
            <div className={`w-10 h-6 rounded-full transition-colors ${ignoreCpu ? 'bg-emerald-500' : 'bg-muted-foreground/30'}`}>
              <div className={`w-5 h-5 mt-0.5 rounded-full bg-white shadow transition-transform ${ignoreCpu ? 'translate-x-[18px]' : 'translate-x-0.5'}`} />
            </div>
          </div>
          <p className="text-xs text-muted-foreground">
            Claude processes can show high CPU even when idle. Enable this to rely only on message analysis for status.
          </p>
        </div>

        <DialogFooter>
          <Button variant="ghost" size="sm" onClick={handleClear}>
            Clear
          </Button>
          <Button size="sm" onClick={handleSave}>
            Save
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function useHotkeyInit() {
  useEffect(() => {
    const savedHotkey = localStorage.getItem(STORAGE_KEY);
    if (savedHotkey) {
      invoke('register_shortcut', { shortcut: savedHotkey }).catch(console.error);
    }
  }, []);
}
