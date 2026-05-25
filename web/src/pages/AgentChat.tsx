import { useState, useEffect, useRef, useCallback } from 'react';
import { Send, AlertCircle, Copy, Check } from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import type { WsMessage } from '@/types/api';
import { WebSocketClient, getOrCreateSessionId } from '@/lib/ws';
import { generateUUID } from '@/lib/uuid';
import { useDraft } from '@/hooks/useDraft';
import { t } from '@/lib/i18n';
import { getSessionMessages } from '@/lib/api';
import ToolCallCard from '@/components/ToolCallCard';
import type { ToolCallInfo } from '@/components/ToolCallCard';
import LetterAvatar from '@/components/brand/LetterAvatar';
import {
  loadChatHistory,
  mapServerMessagesToPersisted,
  persistedToUiMessages,
  saveChatHistory,
  uiMessagesToPersisted,
} from '@/lib/chatHistoryStorage';

/**
 * Assistant identity placeholder. Per BRAND_SPEC_V1.1 §3.8 the chat header
 * should read `{{user_assistant_name}}` (captured at CM051 setup wizard Q7).
 * The B6 wire is in flight — until it lands, fall back to "Your assistant"
 * per V1.1 §8 (non-symmetric fallback rule).
 */
const ASSISTANT_NAME_FALLBACK = 'Your assistant';
const ASSISTANT_SUBTITLE = 'Running locally on Hub';
function assistantInitial(name: string): string {
  const trimmed = name.trim();
  if (!trimmed || trimmed.toLowerCase() === 'your assistant') return 'O';
  return trimmed.slice(0, 1).toUpperCase();
}

interface ChatMessage {
  id: string;
  role: 'user' | 'agent';
  content: string;
  thinking?: string;
  markdown?: boolean;
  toolCall?: ToolCallInfo;
  timestamp: Date;
}

const DRAFT_KEY = 'agent-chat';

export default function AgentChat() {
  const sessionIdRef = useRef(getOrCreateSessionId());
  const { draft, saveDraft, clearDraft } = useDraft(DRAFT_KEY);
  const [messages, setMessages] = useState<ChatMessage[]>(() => {
    // Synchronously hydrate from localStorage so messages survive tab switches
    // without a flash of empty state. Server hydration may override later.
    const persisted = loadChatHistory(sessionIdRef.current);
    return persisted.length > 0 ? persistedToUiMessages(persisted) : [];
  });
  const [historyReady, setHistoryReady] = useState(false);
  const [input, setInput] = useState(draft);
  const [typing, setTyping] = useState(false);
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const wsRef = useRef<WebSocketClient | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const pendingContentRef = useRef('');
  const pendingThinkingRef = useRef('');
  // Snapshot of thinking captured at chunk_reset, so it survives the reset.
  const capturedThinkingRef = useRef('');
  const [streamingContent, setStreamingContent] = useState('');
  const [streamingThinking, setStreamingThinking] = useState('');

  // Persist draft to in-memory store so it survives route changes
  useEffect(() => {
    saveDraft(input);
  }, [input, saveDraft]);

  // Hydrate chat from server (preferred) or localStorage fallback
  useEffect(() => {
    const sid = sessionIdRef.current;
    let cancelled = false;

    (async () => {
      try {
        const res = await getSessionMessages(sid);
        if (cancelled) return;
        if (res.session_persistence && res.messages.length > 0) {
          setMessages((prev) =>
            prev.length > 0 ? prev : persistedToUiMessages(mapServerMessagesToPersisted(res.messages)),
          );
        } else if (!res.session_persistence) {
          setMessages((prev) => {
            if (prev.length > 0) return prev;
            const ls = loadChatHistory(sid);
            return ls.length ? persistedToUiMessages(ls) : prev;
          });
        }
      } catch {
        if (!cancelled) {
          setMessages((prev) => {
            if (prev.length > 0) return prev;
            const ls = loadChatHistory(sid);
            return ls.length ? persistedToUiMessages(ls) : prev;
          });
        }
      } finally {
        if (!cancelled) setHistoryReady(true);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, []);

  // Mirror transcript to localStorage (bounded); server remains source of truth when persistence is on
  useEffect(() => {
    if (!historyReady) return;
    saveChatHistory(sessionIdRef.current, uiMessagesToPersisted(messages));
  }, [messages, historyReady]);

  useEffect(() => {
    const ws = new WebSocketClient();

    ws.onOpen = () => {
      setConnected(true);
      setError(null);
    };

    ws.onClose = (ev: CloseEvent) => {
      setConnected(false);
      if (ev.code !== 1000 && ev.code !== 1001) {
        setError(`Connection closed unexpectedly (code: ${ev.code}). Please check your configuration.`);
      }
    };

    ws.onError = () => {
      setError(t('agent.connection_error'));
    };

    ws.onMessage = (msg: WsMessage) => {
      switch (msg.type) {
        case 'session_start':
        case 'connected':
          break;

        case 'thinking':
          setTyping(true);
          pendingThinkingRef.current += msg.content ?? '';
          setStreamingThinking(pendingThinkingRef.current);
          break;

        case 'chunk':
          setTyping(true);
          pendingContentRef.current += msg.content ?? '';
          setStreamingContent(pendingContentRef.current);
          break;

        case 'chunk_reset':
          // Server signals that the authoritative done message follows.
          // Snapshot thinking before clearing display state.
          capturedThinkingRef.current = pendingThinkingRef.current;
          pendingContentRef.current = '';
          pendingThinkingRef.current = '';
          setStreamingContent('');
          setStreamingThinking('');
          break;

        case 'message':
        case 'done': {
          const content = msg.full_response ?? msg.content ?? pendingContentRef.current;
          const thinking = capturedThinkingRef.current || pendingThinkingRef.current || undefined;
          if (content) {
            setMessages((prev) => [
              ...prev,
              {
                id: generateUUID(),
                role: 'agent',
                content,
                thinking,
                markdown: true,
                timestamp: new Date(),
              },
            ]);
          }
          pendingContentRef.current = '';
          pendingThinkingRef.current = '';
          capturedThinkingRef.current = '';
          setStreamingContent('');
          setStreamingThinking('');
          setTyping(false);
          break;
        }

        case 'tool_call': {
          const toolName = msg.name ?? 'unknown';
          const toolArgs = msg.args;
          setMessages((prev) => {
            // Dedup: backend streaming may re-send tool_call events before execution.
            // Skip if an unresolved card with the same name+args already exists.
            const argsKey = JSON.stringify(toolArgs ?? {});
            const isDuplicate = prev.some(
              (m) => m.toolCall
                && m.toolCall.output === undefined
                && m.toolCall.name === toolName
                && JSON.stringify(m.toolCall.args ?? {}) === argsKey,
            );
            if (isDuplicate) return prev;

            return [
              ...prev,
              {
                id: generateUUID(),
                role: 'agent' as const,
                content: `${t('agent.tool_call_prefix')} ${toolName}(${argsKey})`,
                toolCall: { name: toolName, args: toolArgs },
                timestamp: new Date(),
              },
            ];
          });
          break;
        }

        case 'tool_result': {
          setMessages((prev) => {
            // Forward scan: find the FIRST unresolved toolCall (order-guaranteed by backend)
            const idx = prev.findIndex((m) => m.toolCall && m.toolCall.output === undefined);
            if (idx !== -1) {
              const updated = [...prev];
              const existing = prev[idx]!;
              updated[idx] = {
                ...existing,
                toolCall: { ...existing.toolCall!, output: msg.output ?? '' },
              };
              return updated;
            }
            // Fallback: no unresolved call found — append standalone card
            return [
              ...prev,
              {
                id: generateUUID(),
                role: 'agent' as const,
                content: `${t('agent.tool_result_prefix')} ${msg.output ?? ''}`,
                toolCall: { name: msg.name ?? 'unknown', output: msg.output ?? '' },
                timestamp: new Date(),
              },
            ];
          });
          break;
        }

        case 'cron_result': {
          const cronOutput = msg.output ?? '';
          if (cronOutput) {
            setMessages((prev) => [
              ...prev,
              {
                id: generateUUID(),
                role: 'agent' as const,
                content: cronOutput,
                markdown: true,
                timestamp: new Date(msg.timestamp ?? Date.now()),
              },
            ]);
          }
          break;
        }

        case 'error':
          setMessages((prev) => [
            ...prev,
            {
              id: generateUUID(),
              role: 'agent',
              content: `${t('agent.error_prefix')} ${msg.message ?? t('agent.unknown_error')}`,
              timestamp: new Date(),
            },
          ]);
          if (msg.code === 'AGENT_INIT_FAILED' || msg.code === 'AUTH_ERROR' || msg.code === 'PROVIDER_ERROR') {
            setError(`Configuration error: ${msg.message}. Please check your provider settings (API key, model, etc.).`);
          } else if (msg.code === 'INVALID_JSON' || msg.code === 'UNKNOWN_MESSAGE_TYPE' || msg.code === 'EMPTY_CONTENT') {
            setError(`Message error: ${msg.message}`);
          }
          setTyping(false);
          pendingContentRef.current = '';
          pendingThinkingRef.current = '';
          setStreamingContent('');
          setStreamingThinking('');
          break;
      }
    };

    ws.connect();
    wsRef.current = ws;

    return () => {
      ws.disconnect();
    };
  }, []);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, typing, streamingContent]);

  const handleSend = () => {
    const trimmed = input.trim();
    if (!trimmed || !wsRef.current?.connected) return;

    setMessages((prev) => [
      ...prev,
      {
        id: generateUUID(),
        role: 'user',
        content: trimmed,
        timestamp: new Date(),
      },
    ]);

    try {
      wsRef.current.sendMessage(trimmed);
      setTyping(true);
      pendingContentRef.current = '';
      pendingThinkingRef.current = '';
    } catch {
      setError(t('agent.send_error'));
    }

    setInput('');
    clearDraft();
    if (inputRef.current) {
      inputRef.current.style.height = 'auto';
      inputRef.current.focus();
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const handleTextareaChange = (e: React.ChangeEvent<HTMLTextAreaElement>) => {
    setInput(e.target.value);
    e.target.style.height = 'auto';
    e.target.style.height = `${Math.min(e.target.scrollHeight, 200)}px`;
  };

  const handleCopy = useCallback((msgId: string, content: string) => {
    const onSuccess = () => {
      setCopiedId(msgId);
      setTimeout(() => setCopiedId((prev) => (prev === msgId ? null : prev)), 2000);
    };

    if (navigator.clipboard?.writeText) {
      navigator.clipboard.writeText(content).then(onSuccess).catch(() => {
        // Fallback for insecure contexts (HTTP)
        fallbackCopy(content) && onSuccess();
      });
    } else {
      fallbackCopy(content) && onSuccess();
    }
  }, []);

  /**
   * Fallback copy using a temporary textarea for HTTP contexts
   * where navigator.clipboard is unavailable.
   */
  function fallbackCopy(text: string): boolean {
    const textarea = document.createElement('textarea');
    textarea.value = text;
    textarea.style.position = 'fixed';
    textarea.style.opacity = '0';
    document.body.appendChild(textarea);
    textarea.select();
    try {
      document.execCommand('copy');
      return true;
    } catch {
      return false;
    } finally {
      document.body.removeChild(textarea);
    }
  }

  const assistantName = ASSISTANT_NAME_FALLBACK;
  const assistantLetter = assistantInitial(assistantName);

  return (
    <div
      className="flex flex-col"
      style={{ height: 'calc(100vh - 56px)', background: 'var(--chassis)' }}
    >
      {/* Connection error banner — V1.1 §3.16 default note-box variant. */}
      {error && (
        <div
          className="animate-fade-in"
          style={{
            padding: 'var(--space-2) var(--space-4)',
            background: 'var(--accent-soft)',
            borderBottom: '1px solid var(--accent-soft-2)',
            color: 'var(--accent)',
            fontFamily: 'var(--f-body)',
            fontSize: '13px',
            lineHeight: '20px',
            display: 'flex',
            alignItems: 'center',
            gap: 'var(--space-2)',
          }}
        >
          <AlertCircle className="h-4 w-4 shrink-0" strokeWidth={1.5} />
          {error}
        </div>
      )}

      {/* Messages area — chassis background, oxblood accents. */}
      <div
        className="flex-1 overflow-y-auto"
        style={{ padding: 'var(--space-4)' }}
      >
        <div style={{ maxWidth: '880px', margin: '0 auto' }}>
          {messages.length === 0 && (
            <div
              className="animate-fade-in"
              style={{
                display: 'flex',
                flexDirection: 'column',
                alignItems: 'center',
                justifyContent: 'center',
                textAlign: 'center',
                paddingTop: 'var(--space-7)',
                gap: 'var(--space-3)',
              }}
            >
              <LetterAvatar instance="assistant" letter={assistantLetter} size={56} ariaLabel={assistantName} />
              <p
                style={{
                  margin: 0,
                  fontFamily: 'var(--f-display)',
                  fontWeight: 600,
                  fontSize: '18px',
                  lineHeight: '24px',
                  color: 'var(--ink)',
                }}
              >
                {assistantName}
              </p>
              <p
                style={{
                  margin: 0,
                  fontFamily: 'var(--f-body)',
                  fontSize: '13px',
                  lineHeight: '20px',
                  color: 'var(--ink-muted)',
                  maxWidth: '420px',
                }}
              >
                {t('agent.start_conversation')}
              </p>
            </div>
          )}

          {messages.map((msg, idx) => (
            <div
              key={msg.id}
              className={`group ${msg.role === 'user' ? 'animate-slide-in-right' : 'animate-slide-in-left'}`}
              style={{
                display: 'flex',
                gap: 'var(--space-3)',
                alignItems: 'flex-start',
                marginBottom: 'var(--space-4)',
                animationDelay: `${Math.min(idx * 30, 200)}ms`,
                flexDirection: msg.role === 'user' ? 'row-reverse' : 'row',
              }}
            >
              {/* Letter avatar — Instance A (product) for user, Instance B
                  (assistant) for the agent. Per V1.1 §3.6 the user avatar uses
                  the product mark and the assistant avatar uses the
                  assistant mark. */}
              {msg.role === 'user' ? (
                <LetterAvatar instance="product" letter="O" size={32} ariaLabel="You" />
              ) : (
                <LetterAvatar
                  instance="assistant"
                  letter={assistantLetter}
                  size={32}
                  ariaLabel={assistantName}
                />
              )}

              <div style={{ position: 'relative', maxWidth: '80%' }}>
                {msg.role === 'user' ? (
                  /* User pill — Variant C (cream + ink) per V1.1 §3.7 default. */
                  <div className="user-pill user-pill--C">
                    {msg.markdown ? (
                      <div className="chat-markdown" style={{ fontSize: '14px', lineHeight: '22px' }}>
                        <ReactMarkdown remarkPlugins={[remarkGfm]}>{msg.content}</ReactMarkdown>
                      </div>
                    ) : (
                      <p
                        style={{
                          margin: 0,
                          whiteSpace: 'pre-wrap',
                          wordBreak: 'break-word',
                        }}
                      >
                        {msg.content}
                      </p>
                    )}
                    <p
                      style={{
                        margin: '6px 0 0',
                        fontFamily: 'var(--f-mono)',
                        fontSize: '10px',
                        lineHeight: '14px',
                        textTransform: 'uppercase',
                        letterSpacing: '0.08em',
                        color: 'var(--ink-muted)',
                      }}
                    >
                      {msg.timestamp.toLocaleTimeString()}
                    </p>
                  </div>
                ) : (
                  /* Assistant card — Plex Sans size+weight step per V1.1 §3.8. */
                  <div className="assistant-card">
                    <div className="head">
                      <span className="label">{assistantName}</span>
                      <span className="sub">{ASSISTANT_SUBTITLE}</span>
                    </div>
                    {msg.thinking && (
                      <details style={{ marginBottom: 'var(--space-2)' }}>
                        <summary
                          style={{
                            cursor: 'pointer',
                            userSelect: 'none',
                            fontFamily: 'var(--f-body)',
                            fontSize: '12px',
                            color: 'var(--ink-muted)',
                          }}
                        >
                          Thinking
                        </summary>
                        <pre
                          style={{
                            marginTop: '4px',
                            padding: 'var(--space-2)',
                            borderRadius: 'var(--radius-chip)',
                            background: 'var(--chassis)',
                            color: 'var(--ink-muted)',
                            fontFamily: 'var(--f-mono)',
                            fontSize: '11px',
                            lineHeight: '16px',
                            whiteSpace: 'pre-wrap',
                            wordBreak: 'break-word',
                            overflow: 'auto',
                            maxHeight: '240px',
                          }}
                        >
                          {msg.thinking}
                        </pre>
                      </details>
                    )}
                    {msg.toolCall ? (
                      <ToolCallCard toolCall={msg.toolCall} />
                    ) : msg.markdown ? (
                      <div className="chat-markdown body-cont">
                        <ReactMarkdown remarkPlugins={[remarkGfm]}>{msg.content}</ReactMarkdown>
                      </div>
                    ) : (
                      <p className="body-cont">{msg.content}</p>
                    )}
                    <p
                      style={{
                        margin: 'var(--space-2) 0 0',
                        fontFamily: 'var(--f-mono)',
                        fontSize: '10px',
                        lineHeight: '14px',
                        textTransform: 'uppercase',
                        letterSpacing: '0.08em',
                        color: 'var(--ink-subdued)',
                      }}
                    >
                      {msg.timestamp.toLocaleTimeString()}
                    </p>
                  </div>
                )}

                <button
                  onClick={() => handleCopy(msg.id, msg.content)}
                  aria-label={t('agent.copy_message')}
                  className="opacity-0 group-hover:opacity-100"
                  style={{
                    position: 'absolute',
                    top: 4,
                    right: 4,
                    padding: '6px',
                    borderRadius: 'var(--radius-chip)',
                    background: 'var(--panel)',
                    border: '1px solid var(--hairline)',
                    color: 'var(--ink-muted)',
                    cursor: 'pointer',
                    transition: 'opacity 0.15s ease, color 0.15s ease',
                  }}
                  onMouseEnter={(e) => {
                    e.currentTarget.style.color = 'var(--accent)';
                    e.currentTarget.style.borderColor = 'var(--accent-soft-2)';
                  }}
                  onMouseLeave={(e) => {
                    e.currentTarget.style.color = 'var(--ink-muted)';
                    e.currentTarget.style.borderColor = 'var(--hairline)';
                  }}
                >
                  {copiedId === msg.id ? (
                    <Check className="h-3 w-3" style={{ color: 'var(--status-success)' }} strokeWidth={1.5} />
                  ) : (
                    <Copy className="h-3 w-3" strokeWidth={1.5} />
                  )}
                </button>
              </div>
            </div>
          ))}

          {/* Typing indicator — V1.1 §3.10 */}
          {typing && (
            <div
              className="animate-fade-in"
              style={{
                display: 'flex',
                gap: 'var(--space-3)',
                alignItems: 'flex-start',
                marginBottom: 'var(--space-4)',
              }}
            >
              <LetterAvatar
                instance="assistant"
                letter={assistantLetter}
                size={32}
                ariaLabel={assistantName}
              />
              {streamingContent || streamingThinking ? (
                <div className="assistant-card">
                  <div className="head">
                    <span className="label">{assistantName}</span>
                    <span className="sub">{ASSISTANT_SUBTITLE}</span>
                  </div>
                  {streamingThinking && (
                    <details style={{ marginBottom: 'var(--space-2)' }} open={!streamingContent}>
                      <summary
                        style={{
                          cursor: 'pointer',
                          userSelect: 'none',
                          fontFamily: 'var(--f-body)',
                          fontSize: '12px',
                          color: 'var(--ink-muted)',
                        }}
                      >
                        Thinking{!streamingContent && '...'}
                      </summary>
                      <pre
                        style={{
                          marginTop: '4px',
                          padding: 'var(--space-2)',
                          borderRadius: 'var(--radius-chip)',
                          background: 'var(--chassis)',
                          color: 'var(--ink-muted)',
                          fontFamily: 'var(--f-mono)',
                          fontSize: '11px',
                          lineHeight: '16px',
                          whiteSpace: 'pre-wrap',
                          wordBreak: 'break-word',
                          overflow: 'auto',
                          maxHeight: '240px',
                        }}
                      >
                        {streamingThinking}
                      </pre>
                    </details>
                  )}
                  {streamingContent && <p className="body-cont">{streamingContent}</p>}
                </div>
              ) : (
                <div className="typing">
                  <span className="dot" />
                  <span className="dot" />
                  <span className="dot" />
                </div>
              )}
            </div>
          )}

          <div ref={messagesEndRef} />
        </div>
      </div>

      {/* Input area — chassis-deep dock at the bottom. */}
      <div
        style={{
          borderTop: '1px solid var(--hairline)',
          background: 'var(--chassis-deep)',
          padding: 'var(--space-3) var(--space-4)',
        }}
      >
        <div
          style={{
            display: 'flex',
            alignItems: 'flex-end',
            gap: 'var(--space-2)',
            maxWidth: '880px',
            margin: '0 auto',
          }}
        >
          <textarea
            ref={inputRef}
            rows={1}
            value={input}
            onChange={handleTextareaChange}
            onKeyDown={handleKeyDown}
            placeholder={connected ? t('agent.type_message') : t('agent.connecting')}
            disabled={!connected}
            style={{
              flex: 1,
              minHeight: '44px',
              maxHeight: '200px',
              padding: '10px 14px',
              background: 'var(--panel)',
              border: '1px solid var(--hairline-soft)',
              borderRadius: 'var(--radius-control)',
              color: 'var(--ink)',
              fontFamily: 'var(--f-body)',
              fontSize: '14px',
              lineHeight: '22px',
              resize: 'none',
              opacity: connected ? 1 : 0.5,
            }}
          />
          <button
            type="button"
            onClick={handleSend}
            disabled={!connected || !input.trim()}
            aria-label="Send"
            style={{
              flexShrink: 0,
              width: '44px',
              height: '44px',
              borderRadius: '9999px',
              background: 'var(--accent)',
              color: '#fff',
              border: 'none',
              cursor: connected && input.trim() ? 'pointer' : 'not-allowed',
              opacity: connected && input.trim() ? 1 : 0.4,
              display: 'inline-flex',
              alignItems: 'center',
              justifyContent: 'center',
              transition: 'background-color 0.15s ease',
            }}
            onMouseEnter={(e) => {
              if (connected && input.trim()) e.currentTarget.style.background = 'var(--accent-hover)';
            }}
            onMouseLeave={(e) => {
              e.currentTarget.style.background = 'var(--accent)';
            }}
          >
            <Send className="h-5 w-5" strokeWidth={1.5} />
          </button>
        </div>
        <div
          style={{
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            marginTop: 'var(--space-2)',
            gap: '6px',
          }}
        >
          {/* Status dot — no glow per V1.1 Don'ts #6. */}
          <span
            className="status-dot"
            style={{
              background: connected ? 'var(--status-success)' : 'var(--ink-faint)',
            }}
            aria-hidden
          />
          <span
            style={{
              fontFamily: 'var(--f-mono)',
              fontSize: '10px',
              lineHeight: '14px',
              textTransform: 'uppercase',
              letterSpacing: '0.08em',
              color: 'var(--ink-muted)',
            }}
          >
            {connected ? t('agent.connected_status') : t('agent.disconnected_status')}
          </span>
        </div>
      </div>
    </div>
  );
}
