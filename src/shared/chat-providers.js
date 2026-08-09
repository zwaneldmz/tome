// Assistant chat providers: the names, wire shapes, endpoints, and default
// model ids for the assistant pane. Names + shapes only — NO secrets here;
// keys live in the user's login shell and are resolved in main
// (lib/chat-client.js), never in the renderer.
//
// `wire` selects the transport dialect:
//   'openai'    — OpenAI-compatible /chat/completions SSE (Kimi, GLM)
//   'anthropic' — the Anthropic SDK's beta.messages.stream (Claude)
// `keyEnv` names the login-shell env var main looks the key up under.
export const CHAT_PROVIDERS = {
  kimi: {
    label: 'Kimi (Moonshot)',
    wire: 'openai',
    baseURL: 'https://api.moonshot.ai/v1',
    // The provider's own model id. The user asked for "Kimi K3"; if Moonshot
    // spells the id differently it is user-overridable in Preferences
    // (⌘, → Assistant) or via TOME_CHAT_MODEL.
    model: 'kimi-k3',
    keyEnv: 'MOONSHOT_API_KEY',
  },
  glm: {
    label: 'GLM (Zhipu)',
    wire: 'openai',
    baseURL: 'https://open.bigmodel.cn/api/paas/v4',
    // The provider's own model id. The user asked for "GLM 5.2"; if Zhipu
    // spells the id differently it is user-overridable in Preferences
    // (⌘, → Assistant) or via TOME_CHAT_MODEL.
    model: 'glm-5.2',
    keyEnv: 'ZHIPU_API_KEY',
  },
  claude: {
    label: 'Claude (Anthropic)',
    wire: 'anthropic',
    baseURL: null, // the SDK knows its own endpoint
    // The provider's own model id — the pre-refactor ANTHROPIC_MODEL
    // default in src/main/index.js. User-overridable in Preferences
    // (⌘, → Assistant) or via TOME_CHAT_MODEL.
    model: 'claude-opus-5',
    keyEnv: 'ANTHROPIC_API_KEY',
  },
}

export const DEFAULT_CHAT_PROVIDER = 'kimi'
