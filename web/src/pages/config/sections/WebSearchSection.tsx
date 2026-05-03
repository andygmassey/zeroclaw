import { Search } from 'lucide-react';
import SectionCard from '../controls/SectionCard';
import FieldRow from '../controls/FieldRow';
import Select from '../controls/Select';
import Slider from '../controls/Slider';
import NumberInput from '../controls/NumberInput';
import TextInput from '../controls/TextInput';
import { t } from '@/lib/i18n';

interface Props {
  config: Record<string, unknown>;
  onUpdate: (field: string, value: unknown) => void;
}

// Vane is listed first because it is the default provider for fresh
// installs from CM051 #32 forward (the bundled local container at
// localhost:3000). DuckDuckGo, Brave, and SearXNG remain configurable
// alternatives.
const PROVIDER_OPTIONS = [
  { value: 'vane', label: 'Vane (local, default)' },
  { value: 'duckduckgo', label: 'DuckDuckGo' },
  { value: 'brave', label: 'Brave' },
  { value: 'searxng', label: 'SearXNG' },
];

const DEFAULT_VANE_URL = 'http://localhost:3000';

export default function WebSearchSection({ config, onUpdate }: Props) {
  const ws = (config.web_search as Record<string, unknown>) ?? {};
  const provider = (ws.provider as string) ?? 'vane';

  return (
    <SectionCard
      icon={<Search className="h-5 w-5" />}
      title={t('config.section.web_search')}
      enabled={(ws.enabled as boolean) ?? true}
      onToggleEnabled={(v) => onUpdate('web_search.enabled', v)}
    >
      <FieldRow label={t('config.field.web_search_provider')} description={t('config.field.web_search_provider.desc')}>
        <Select
          value={provider}
          onChange={(v) => onUpdate('web_search.provider', v)}
          options={PROVIDER_OPTIONS}
        />
      </FieldRow>
      {provider === 'vane' && (
        <FieldRow
          label={t('config.field.vane_url')}
          description={t('config.field.vane_url.desc')}
        >
          <TextInput
            value={(ws.vane_url as string) ?? DEFAULT_VANE_URL}
            onChange={(v) => onUpdate('web_search.vane_url', v)}
            placeholder={DEFAULT_VANE_URL}
          />
        </FieldRow>
      )}
      <FieldRow label={t('config.field.max_results')} description={t('config.field.max_results.desc')}>
        <Slider
          value={(ws.max_results as number) ?? 5}
          onChange={(v) => onUpdate('web_search.max_results', v)}
          min={1}
          max={10}
        />
      </FieldRow>
      <FieldRow label={t('config.field.web_search_timeout')} description={t('config.field.web_search_timeout.desc')}>
        <NumberInput
          value={(ws.timeout_secs as number) ?? 15}
          onChange={(v) => onUpdate('web_search.timeout_secs', v)}
          min={1}
        />
      </FieldRow>
    </SectionCard>
  );
}
