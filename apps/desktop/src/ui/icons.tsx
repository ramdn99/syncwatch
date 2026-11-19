// The ONE icon module (Milestone 8): every icon in the app goes through this map, so icons can
// be swapped in one place. Lucide (lucide-react 1.66.0, ISC; Feather-derived icons MIT, see
// third_party/licenses/lucide-react-LICENSE.txt), named imports only (tree-shaken, bundled, no
// CDN). Rules: fixed sizes, one stroke width, currentColor (follows the theme tokens),
// aria-hidden when decorative, an accessible name when the icon is the only content.
import {
  ArrowLeft,
  Ban,
  ChevronDown,
  ChevronRight,
  ChevronsLeft,
  ChevronsRight,
  Circle,
  CircleAlert,
  CircleCheck,
  Copy,
  Crown,
  Download,
  Ellipsis,
  EyeOff,
  FileQuestion,
  FileWarning,
  Keyboard,
  Link,
  LoaderCircle,
  Lock,
  Maximize2,
  MessageSquareOff,
  Minimize2,
  Pause,
  Play,
  Plus,
  RefreshCw,
  Server,
  UserMinus,
  UserPlus,
  Volume2,
  VolumeX,
  X,
  type LucideIcon,
} from 'lucide-react';

export const ICONS = {
  play: Play,
  pause: Pause,
  volume: Volume2,
  muted: VolumeX,
  fullscreen: Maximize2,
  exitFullscreen: Minimize2,
  close: X,
  more: Ellipsis,
  lock: Lock,
  expand: ChevronRight,
  collapse: ChevronDown,
  collapseLeft: ChevronsLeft,
  collapseRight: ChevronsRight,
  back: ArrowLeft,
  add: Plus,
  selected: Circle,
  host: Crown,
  // Connection state (each with its own shape, always next to text).
  direct: Link,
  relayed: Server,
  reconnecting: RefreshCw,
  // Media status.
  ready: CircleCheck,
  buffering: LoaderCircle,
  missingFile: FileQuestion,
  mismatch: FileWarning,
  downloading: Download,
  complete: CircleCheck,
  warning: CircleAlert,
  // Moderation and chat.
  chatMuted: MessageSquareOff,
  mutedByYou: EyeOff,
  copy: Copy,
  kick: UserMinus,
  invite: UserPlus,
  ban: Ban,
  shortcuts: Keyboard,
} as const satisfies Record<string, LucideIcon>;

export type IconName = keyof typeof ICONS;

/** The size scale (px). */
export const ICON_SIZE = { xs: 14, sm: 16, md: 20, lg: 24 } as const;
export const ICON_STROKE = 2;

/**
 * An icon. Decorative by default (`aria-hidden`); pass `label` when the icon carries meaning on
 * its own (then it is announced as an image with that name).
 */
export function Icon({ name, size = 'sm', label, className }: { name: IconName; size?: keyof typeof ICON_SIZE; label?: string; className?: string }) {
  const C = ICONS[name];
  const px = ICON_SIZE[size];
  return label ? (
    <C size={px} strokeWidth={ICON_STROKE} role="img" aria-label={label} focusable="false" className={`inline-block shrink-0 ${className ?? ''}`} data-icon={name} />
  ) : (
    <C size={px} strokeWidth={ICON_STROKE} aria-hidden="true" focusable="false" className={`inline-block shrink-0 ${className ?? ''}`} data-icon={name} />
  );
}
