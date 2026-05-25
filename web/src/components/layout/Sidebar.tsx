/**
 * Hub sidebar — V1.1 §5.1
 *
 * Brand element (harvested from OstlerInstaller / SidebarView.swift lines 14-30):
 *   - 28x28 circular "O" (oxblood fill, white letter) via LetterAvatar Instance A
 *   - OSTLER wordmark in Outfit SemiBold 18 UPPERCASE tracked 0.10em
 *   - "Personal assistant" subtitle in Plex Mono UPPERCASE 10/12
 *   - 1px hairline divider beneath
 *
 * Nav: 9 items locked to Lucide icon names per V1.1 §5.1 nav table.
 *   1. Home          /            home
 *   2. Chat          /chat        message-square
 *   3. Timeline      /timeline    clock
 *   4. People        /people      users
 *   5. {{first_name}}pedia /wiki  book-open
 *   6. Memory        /memory      brain
 *   7. Integrations  /integrations puzzle   ← V1.1 Fixup 1 overrides spec `plug`
 *   8. Health        /health      activity
 *   9. Settings      /settings    settings
 *
 * Hidden from nav (routes kept, URL-accessible): /cost /tools /cron /logs /canvas /pairing
 *
 * Active state: --accent-soft background + --accent text/icon.
 * Hover state:  --chassis background + --ink text/icon.
 *
 * Per V1.1 Don'ts #5: no ZeroClaw branding in customer surfaces.
 */

import { NavLink } from 'react-router-dom';
import {
  Home,
  MessageSquare,
  Clock,
  Users,
  BookOpen,
  Brain,
  Puzzle,
  Activity,
  Settings,
  Wrench,
  DollarSign,
  Monitor,
} from 'lucide-react';
import { t } from '@/lib/i18n';
import { featureFlags } from '@/config/features';
import LetterAvatar from '@/components/brand/LetterAvatar';

interface NavItem {
  to: string;
  icon: typeof Home;
  labelKey: string;
  /** Fallback English label used if the i18n key isn't translated. */
  labelFallback: string;
  /** Reversibility flag (CX-81 B4+B5 pattern). false hides from rendered nav
   * but keeps the route entry in the codebase. Never delete entries here. */
  show: boolean;
}

// Full nav definition. Items are filtered by feature flags at render-time so
// the code path stays present and reversible — never delete entries here.
// V1.1 §5.1 9-item nav + legacy items hidden behind show:false for reversibility.
const allNavItems: NavItem[] = [
  // V1.1 §5.1 nav surface (9 items, all show:true)
  { to: '/',             icon: Home,          labelKey: 'nav.home',         labelFallback: 'Home',         show: true },
  { to: '/chat',         icon: MessageSquare, labelKey: 'nav.chat',         labelFallback: 'Chat',         show: true },
  { to: '/timeline',     icon: Clock,         labelKey: 'nav.timeline',     labelFallback: 'Timeline',     show: true },
  { to: '/people',       icon: Users,         labelKey: 'nav.people',       labelFallback: 'People',       show: true },
  { to: '/wiki',         icon: BookOpen,      labelKey: 'nav.wiki',         labelFallback: 'Personal wiki', show: true },
  { to: '/memory',       icon: Brain,         labelKey: 'nav.memory',       labelFallback: 'Memory',       show: true },
  // Fixup 1: Integrations icon = puzzle (NOT plug). V1.1 spec table at §5.1
  // says `plug`; the fixups doc explicitly overrides to `puzzle`. Per brief
  // "fixups doc is authoritative overrides over the spec."
  { to: '/integrations', icon: Puzzle,        labelKey: 'nav.integrations', labelFallback: 'Integrations', show: true },
  { to: '/health',       icon: Activity,      labelKey: 'nav.health',       labelFallback: 'Health',       show: true },
  { to: '/settings',     icon: Settings,      labelKey: 'nav.settings',     labelFallback: 'Settings',     show: true },

  // Legacy entries hidden from V1.1 nav but kept for reversibility (CX-81 B4+B5
  // pattern). Routes remain URL-accessible per V1.1 §5.1 "Hidden from nav".
  { to: '/tools',        icon: Wrench,        labelKey: 'nav.tools',        labelFallback: 'Tools',        show: false },
  { to: '/cron',         icon: Clock,         labelKey: 'nav.cron',         labelFallback: 'Scheduled Jobs', show: false },
  { to: '/cost',         icon: DollarSign,    labelKey: 'nav.cost',         labelFallback: 'Cost Tracker', show: featureFlags.v1_show_cost },
  { to: '/logs',         icon: Activity,      labelKey: 'nav.logs',         labelFallback: 'Logs',         show: false },
  { to: '/canvas',       icon: Monitor,       labelKey: 'nav.canvas',       labelFallback: 'Canvas',       show: featureFlags.v1_show_canvas_in_nav },
];

const navItems = allNavItems.filter((item) => item.show);

/** Translate via i18n, fall back to the locked English label. */
function navLabel(item: NavItem): string {
  const translated = t(item.labelKey);
  // `t` returns the key itself when no translation exists. Detect this and
  // substitute the brand-correct English fallback so the sidebar reads
  // cleanly on day-one even though i18n strings for the new keys may not
  // yet be in the catalogue.
  return translated === item.labelKey ? item.labelFallback : translated;
}

// Shared nav item sub-component — eliminates duplication between mobile & desktop nav.
function SidebarNavItem({
  item,
  showLabel,
  showTooltip,
  onClick,
}: {
  item: NavItem;
  showLabel: boolean;
  showTooltip: boolean;
  onClick: () => void;
}) {
  const { to, icon: Icon } = item;
  const label = navLabel(item);
  return (
    <NavLink
      key={to}
      to={to}
      end={to === '/'}
      onClick={onClick}
      className={({ isActive }) =>
        [
          'flex items-center rounded-lg text-sm font-medium transition-colors group relative',
          showLabel ? 'justify-start gap-3 px-3 py-2' : 'justify-center w-10 h-10 mx-auto',
          isActive
            ? 'text-(--accent)'
            : 'text-(--ink) hover:text-(--ink) hover:bg-(--chassis)',
        ].join(' ')
      }
      style={({ isActive }) => ({
        ...(isActive ? { background: 'var(--accent-soft)' } : {}),
        fontFamily: 'var(--f-body)',
      })}
    >
      {({ isActive }) => (
        <>
          <Icon
            className="h-[18px] w-[18px] shrink-0 transition-colors"
            strokeWidth={1.5}
            style={{ color: isActive ? 'var(--accent)' : 'var(--ink)' }}
          />
          {showLabel && <span className="whitespace-nowrap">{label}</span>}
          {showTooltip && (
            <span
              className="absolute left-full ml-2 px-2 py-1 rounded-md text-xs whitespace-nowrap opacity-0 group-hover:opacity-100 transition-opacity pointer-events-none z-9999"
              style={{
                background: 'var(--panel)',
                color: 'var(--ink)',
                border: '1px solid var(--hairline)',
                boxShadow: 'var(--shadow-soft)',
              }}
            >
              {label}
            </span>
          )}
        </>
      )}
    </NavLink>
  );
}

interface SidebarProps {
  open: boolean;
  onClose: () => void;
  collapsed: boolean;
}

export default function Sidebar({ open, onClose, collapsed }: SidebarProps) {
  return (
    <>
      {/* Backdrop — mobile only */}
      {open && (
        <div
          className="md:hidden fixed inset-0 z-40"
          style={{ background: 'rgba(20, 18, 14, 0.45)' }}
          onClick={onClose}
          onKeyDown={(e) => {
            if (e.key === 'Escape') onClose();
          }}
          role="button"
          tabIndex={-1}
          aria-label="Close menu"
        />
      )}

      {/* Desktop sidebar — collapsible */}
      <aside
        className="hidden md:flex fixed top-0 left-0 h-screen flex-col z-50 transition-all duration-200 ease-in-out"
        style={{
          background: 'var(--chassis-deep)',
          borderRight: '1px solid var(--hairline)',
          width: collapsed ? '56px' : '240px',
        }}
        aria-label={collapsed ? 'Collapsed sidebar' : 'Main sidebar'}
      >
        <SidebarBrand collapsed={collapsed} />
        <nav className="flex-1 overflow-y-auto py-3 px-2 space-y-1">
          {navItems.map((item) => (
            <SidebarNavItem
              key={item.to}
              item={item}
              showLabel={!collapsed}
              showTooltip={collapsed}
              onClick={onClose}
            />
          ))}
        </nav>
        <SidebarFooter collapsed={collapsed} layout="desktop" />
      </aside>

      {/* Mobile sidebar — slides in/out */}
      <aside
        className={[
          'md:hidden fixed top-0 left-0 h-screen w-60 flex flex-col z-50 transition-transform duration-200 ease-out',
          open ? 'translate-x-0' : '-translate-x-full',
        ].join(' ')}
        style={{
          background: 'var(--chassis-deep)',
          borderRight: '1px solid var(--hairline)',
        }}
        aria-label="Mobile menu"
      >
        <SidebarBrand collapsed={false} />
        <nav className="flex-1 overflow-y-auto py-3 px-2 space-y-1">
          {navItems.map((item) => (
            <SidebarNavItem
              key={item.to}
              item={item}
              showLabel
              showTooltip={false}
              onClick={onClose}
            />
          ))}
        </nav>
        <SidebarFooter collapsed={false} layout="mobile" />
      </aside>
    </>
  );
}

/**
 * Brand element at the top of the sidebar. Pattern harvested from
 * OstlerInstaller / SidebarView.swift lines 14-30.
 *   28x28 circular oxblood "O" + OSTLER wordmark + "Personal assistant"
 *   subtitle, with a hairline divider beneath.
 */
function SidebarBrand({ collapsed }: { collapsed: boolean }) {
  return (
    <div
      className="flex items-center shrink-0 overflow-hidden"
      style={{
        borderBottom: '1px solid var(--hairline)',
        height: '64px',
        padding: collapsed ? '0 14px' : '0 var(--space-3)',
        gap: collapsed ? '0' : 'var(--space-2)',
      }}
    >
      <LetterAvatar instance="product" letter="O" size={28} ariaLabel="Ostler" />
      <div
        className="flex flex-col transition-opacity duration-200"
        style={{
          opacity: collapsed ? 0 : 1,
          pointerEvents: collapsed ? 'none' : 'auto',
          minWidth: 0,
        }}
      >
        <span
          className="whitespace-nowrap"
          style={{
            fontFamily: 'var(--f-display)',
            fontWeight: 600,
            fontSize: '18px',
            lineHeight: '22px',
            textTransform: 'uppercase',
            letterSpacing: '0.10em',
            color: 'var(--ink)',
          }}
        >
          OSTLER
        </span>
        <span
          className="whitespace-nowrap"
          style={{
            fontFamily: 'var(--f-mono)',
            fontWeight: 400,
            fontSize: '10px',
            lineHeight: '12px',
            textTransform: 'uppercase',
            letterSpacing: '0.08em',
            color: 'var(--ink-muted)',
            marginTop: '2px',
          }}
        >
          Personal assistant
        </span>
      </div>
    </div>
  );
}

function SidebarFooter({
  collapsed,
  layout,
}: {
  collapsed: boolean;
  layout: 'desktop' | 'mobile';
}) {
  // V1.1 customer surfaces don't expose "ZeroClaw" as branding (Don'ts #5).
  // Footer reads as a neutral wordmark line so the customer always sees
  // "OSTLER" at top and a quiet repeat at bottom.
  const footerText = 'Ostler Hub';

  if (layout === 'mobile') {
    return (
      <div
        className="px-4 py-3"
        style={{
          borderTop: '1px solid var(--hairline)',
          fontFamily: 'var(--f-mono)',
          fontSize: '10px',
          lineHeight: '14px',
          textTransform: 'uppercase',
          letterSpacing: '0.10em',
          color: 'var(--ink-muted)',
        }}
      >
        {footerText}
      </div>
    );
  }
  return (
    <div
      className="shrink-0 whitespace-nowrap overflow-hidden transition-opacity duration-200"
      style={{
        borderTop: '1px solid var(--hairline)',
        padding: collapsed ? '12px 0' : '14px var(--space-3)',
        fontFamily: 'var(--f-mono)',
        fontSize: '10px',
        color: 'var(--ink-muted)',
        textTransform: 'uppercase',
        letterSpacing: '0.10em',
        opacity: collapsed ? 0 : 1,
        textAlign: collapsed ? 'center' : 'left',
      }}
    >
      {!collapsed && footerText}
    </div>
  );
}
