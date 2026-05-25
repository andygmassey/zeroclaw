import {
  Settings,
  Save,
  CheckCircle,
  AlertTriangle,
  ShieldAlert,
  Code,
  SlidersHorizontal,
} from 'lucide-react';
import { t } from '@/lib/i18n';
import { useConfigState } from './config/useConfigState';
import ConfigFormView from './config/ConfigFormView';
import ConfigTomlEditor from './config/ConfigTomlEditor';

export default function Config() {
  const {
    parsedConfig,
    rawToml,
    mode,
    loading,
    saving,
    error,
    success,
    parseError,
    updateField,
    switchMode,
    updateRawToml,
    save,
  } = useConfigState();

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 rounded-full animate-spin" style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }} />
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full p-6 gap-4 animate-fade-in overflow-hidden">
      {/* Header */}
      <div className="flex items-center justify-between flex-shrink-0">
        <div className="flex items-center gap-2">
          <Settings className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
          <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>{t('config.configuration_title')}</h2>
        </div>
        <div className="flex items-center gap-3">
          {/* Mode toggle */}
          <div className="flex rounded-xl overflow-hidden border" style={{ borderColor: 'var(--pc-border)' }}>
            <button
              type="button"
              onClick={() => switchMode('form')}
              className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium transition-colors"
              style={{
                background: mode === 'form' ? 'var(--pc-accent)' : 'var(--pc-bg-surface)',
                color: mode === 'form' ? 'white' : 'var(--pc-text-secondary)',
              }}
            >
              <SlidersHorizontal className="h-3.5 w-3.5" />
              {t('config.mode.form')}
            </button>
            <button
              type="button"
              onClick={() => switchMode('advanced')}
              className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium transition-colors"
              style={{
                background: mode === 'advanced' ? 'var(--pc-accent)' : 'var(--pc-bg-surface)',
                color: mode === 'advanced' ? 'white' : 'var(--pc-text-secondary)',
              }}
            >
              <Code className="h-3.5 w-3.5" />
              {t('config.mode.advanced')}
            </button>
          </div>

          <button onClick={save} disabled={saving} className="btn-electric flex items-center gap-2 text-sm px-4 py-2">
            <Save className="h-4 w-4" />{saving ? t('config.saving') : t('config.save')}
          </button>
        </div>
      </div>

      {/* Sensitive fields note — V1.1 §3.16 warning note-box variant */}
      <div className="note-box note-box--warning flex-shrink-0 flex items-start gap-3">
        <ShieldAlert className="h-5 w-5 flex-shrink-0 mt-0.5" style={{ color: 'var(--accent-warm-amber)' }} strokeWidth={1.5} />
        <div>
          <p
            style={{
              margin: 0,
              fontFamily: 'var(--f-body)',
              fontSize: '13px',
              fontWeight: 500,
              lineHeight: '20px',
              color: 'var(--ink)',
            }}
          >
            {t('config.sensitive_title')}
          </p>
          <p
            style={{
              margin: '2px 0 0',
              fontFamily: 'var(--f-body)',
              fontSize: '13px',
              lineHeight: '20px',
              color: 'var(--ink-muted)',
            }}
          >
            {t('config.sensitive_hint')}
          </p>
        </div>
      </div>

      {/* Success message — V1.1 §3.16 success note-box variant */}
      {success && (
        <div className="note-box note-box--success animate-fade-in flex-shrink-0 flex items-center gap-2">
          <CheckCircle className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--accent-3)' }} strokeWidth={1.5} />
          <span style={{ fontFamily: 'var(--f-body)', fontSize: '13px', color: 'var(--ink)' }}>{success}</span>
        </div>
      )}

      {/* Error / parse error message — V1.1 §3.16 default note-box variant */}
      {(error || parseError) && (
        <div className="note-box note-box--default animate-fade-in flex-shrink-0 flex items-center gap-2">
          <AlertTriangle className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--accent)' }} strokeWidth={1.5} />
          <span style={{ fontFamily: 'var(--f-body)', fontSize: '13px', color: 'var(--ink)' }}>{error || parseError}</span>
        </div>
      )}

      {/* Content: Form or TOML editor */}
      {mode === 'form' ? (
        <ConfigFormView config={parsedConfig} onUpdate={updateField} />
      ) : (
        <ConfigTomlEditor value={rawToml} onChange={updateRawToml} />
      )}
    </div>
  );
}
