import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from '@/components/ui/dialog';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
import { DisplaySettings, DisplayItemKey } from '../types/session';

interface SettingsProps {
  isOpen: boolean;
  onClose: () => void;
  displaySettings: DisplaySettings;
  onDisplaySettingsChange: (settings: DisplaySettings) => void;
}

const STORAGE_KEY = 'claude-sessions-hotkey';
const DEFAULT_HOTKEY = 'Control+Space';

const DISPLAY_LABELS: Record<DisplayItemKey, string> = {
  pid: 'PID',
  tty: 'TTY',
  cpu: 'CPU usage',
  time: 'Last updated',
};

function DisplayItemList({
  items,
  onChange,
}: {
  items: DisplaySettings;
  onChange: (items: DisplaySettings) => void;
}) {
  const dragItem = useRef<number | null>(null);
  const dragOverItem = useRef<number | null>(null);

  const handleDragStart = (index: number) => {
    dragItem.current = index;
  };

  const handleDragOver = (e: React.DragEvent, index: number) => {
    e.preventDefault();
    dragOverItem.current = index;
  };

  const handleDrop = () => {
    if (dragItem.current === null || dragOverItem.current === null) return;
    if (dragItem.current === dragOverItem.current) return;
    const reordered = [...items];
    const [moved] = reordered.splice(dragItem.current, 1);
    reordered.splice(dragOverItem.current, 0, moved);
    dragItem.current = null;
    dragOverItem.current = null;
    onChange(reordered);
  };

  const toggleItem = (index: number) => {
    const updated = items.map((item, i) =>
      i === index ? { ...item, enabled: !item.enabled } : item
    );
    onChange(updated);
  };

  return (
    <div className="space-y-2">
      <label className="text-sm font-medium text-foreground">
        Session Card Details
      </label>
      <div className="space-y-1">
        {items.map((item, index) => (
          <div
            key={item.key}
            draggable
            onDragStart={() => handleDragStart(index)}
            onDragOver={(e) => handleDragOver(e, index)}
            onDrop={handleDrop}
            className="flex items-center gap-2 p-1.5 rounded cursor-grab active:cursor-grabbing hover:bg-muted/50 select-none"
          >
            <svg className="w-3.5 h-3.5 text-muted-foreground shrink-0" viewBox="0 0 16 16" fill="currentColor">
              <circle cx="5" cy="3" r="1.5" />
              <circle cx="11" cy="3" r="1.5" />
              <circle cx="5" cy="8" r="1.5" />
              <circle cx="11" cy="8" r="1.5" />
              <circle cx="5" cy="13" r="1.5" />
              <circle cx="11" cy="13" r="1.5" />
            </svg>
            <Checkbox
              checked={item.enabled}
              onCheckedChange={() => toggleItem(index)}
            />
            <span className="text-sm text-foreground">{DISPLAY_LABELS[item.key]}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

export function Settings({ isOpen, onClose, displaySettings, onDisplaySettingsChange }: SettingsProps) {
  const [hotkey, setHotkey] = useState(DEFAULT_HOTKEY);
  const [isRecording, setIsRecording] = useState(false);
  const [recordedKeys, setRecordedKeys] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  // Load saved hotkey on mount
  useEffect(() => {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved) {
      setHotkey(saved);
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

        <DisplayItemList
          items={displaySettings}
          onChange={onDisplaySettingsChange}
        />

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
