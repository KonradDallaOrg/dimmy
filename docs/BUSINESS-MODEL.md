# Dimmy - Business Model

**Date:** 2026-02-23
**Orientation:** Open-source core (GitHub) + optional paid cloud tier

---

## 1. Model: Open Core + Cloud Services

Based on analysis of Obsidian ($25M ARR, 18 employees), Plausible Analytics ($100K MRR in 3 years), and Vikunja (bootstrapped, sustainable), the **open core model** is the optimal fit:

| Principle | Implementation |
|-----------|---------------|
| **Free core is fully functional** | Local transcription, all features, no artificial limits |
| **Paid adds genuine cloud value** | API proxy, AI post-processing, sync, team features |
| **No vendor lock-in** | Users can always BYO API keys for free |
| **Trust = willingness to pay** | OSS transparency increases conversion, not decreases it |

---

## 2. Pricing Tiers

### Free (Open Source)
- Local Whisper transcription (all model sizes)
- BYO API key support (Groq, OpenAI, custom)
- Voice commands, smart formatting
- Clipboard history (local SQLite)
- Custom vocabulary
- Noise preprocessing (RNNoise)
- All platforms, all features, forever

### Pro ($9/month or $89 lifetime)
- **Cloud API proxy** — No API key hassle, just works
- **AI post-processing** — Clean, Professional, Custom modes via cloud LLM
- **Priority transcription** — Faster cloud processing
- **Multi-device sync** — Transcription history across machines
- **Advanced export** — DOCX, Notion markdown, SRT
- **Email support**

### Team ($25/user/month)
- Everything in Pro
- Shared transcription library
- Role-based access
- SSO/SAML
- Audit logging
- Custom vocabulary shared across team
- Priority support + SLA

### Enterprise (Custom pricing)
- On-premise deployment
- White-label licensing
- Dedicated infrastructure
- Custom model training/fine-tuning
- $5,000-50,000/year depending on volume

---

## 3. Cost Structure

### API Costs (what we pay)

| Provider | Model | Cost/Hour | Notes |
|----------|-------|-----------|-------|
| Groq | Whisper Large v3 Turbo | $0.04 | Free tier: 8h audio/day |
| Groq | Whisper Large v3 | $0.111 | Higher accuracy |
| OpenAI | Whisper-1 | $0.36 | Most widely used |
| OpenAI | GPT-4o-mini (post-processing) | ~$0.01/summary | For AI cleanup |

### Pro Tier Margin

If Pro users average 2 hours transcription/day:
- Our cost: ~$0.08/day (Groq Turbo) + $0.05/day (LLM cleanup) = $0.13/day = **~$4/month**
- Revenue: $9/month
- **Gross margin: ~55%**

With OpenAI backend:
- Our cost: ~$0.72/day + $0.05/day = $0.77/day = **~$23/month** (unprofitable at $9)
- **Solution:** Default Pro tier to Groq, offer OpenAI as optional at $19/month

### Infrastructure Costs

| Phase | Monthly Cost | Notes |
|-------|-------------|-------|
| Phase 1 (0-6mo) | $0 | Everything local, no server |
| Phase 2 (6-12mo) | ~$200 | API costs for early Pro users |
| Phase 3 (12-24mo) | ~$600 | Growing Pro base |
| Phase 4 (24mo+) | ~$2,000+ | Scale; consider self-hosting at 500+ Pro users |

Self-hosting break-even: ~$826-1,400/month for GPU server, justified only at 20,000+ transcription hours/month.

---

## 4. Financial Projections

Using industry-standard SaaS freemium conversion rates (2.6-5.0%, source: First Page Sage 2026):

| Phase | Timeline | MAU | Conversion | Paying | MRR | Costs | Net MRR |
|-------|----------|-----|-----------|--------|-----|-------|---------|
| Launch | 0-6mo | 1,000 | — | 0 | $0 | $0 | $0 |
| Freemium | 6-12mo | 5,000 | 2.6% | 130 | $1,170 | $200 | **$970** |
| Growth | 12-24mo | 15,000 | 3.7% | 555 | $4,995 | $600 | **$4,395** |
| Scale | 24-36mo | 50,000 | 5.0% | 2,500 | $22,500 | $2,000 | **$20,500** |

### Key Milestones

| Milestone | When | Revenue |
|-----------|------|---------|
| **Ramen profitable** (1 dev) | 8-12 months | ~$1,200/mo |
| **Comfortable** (1 dev) | 18-24 months | ~$5,000/mo |
| **Small team** (2-3 devs) | 24-36 months | ~$20,000/mo |
| **Real business** | 36+ months | $60,000+/mo |

---

## 5. Distribution Strategy

### Primary: GitHub + Direct Download

| Channel | Audience | Effort | Priority |
|---------|----------|--------|----------|
| **GitHub Releases** | Developers, early adopters | Low | P0 |
| **Project website** | General users | Medium | P0 |
| **Homebrew** (macOS) | Mac developers | Low | P1 |
| **Winget** (Windows) | Windows power users | Low | P1 |
| **Chocolatey** (Windows) | Sysadmins | Low | P2 |
| **Flatpak/Snap** (Linux) | Linux users | Medium | P2 |

### App Stores: Defer

- **Microsoft Store:** 15% commission, sandboxing may conflict with local model loading
- **Mac App Store:** 30% commission, strict sandboxing, notarization required regardless
- **Verdict:** Distribute directly + package managers. App stores only later if sandbox allows.

### Community

| Channel | Purpose |
|---------|---------|
| GitHub Discussions | Feature requests, bug reports, community support |
| Discord | Real-time community, beta testing |
| GitHub Sponsors | Supplementary income ($100-500/mo realistic) |

---

## 6. Growth Strategy

### Phase 1: Build Trust (Months 0-6)
- Ship fully functional free app on GitHub
- Target: 500-1,000 GitHub stars
- Content: Blog posts, YouTube demo, HackerNews launch
- Community: Discord + GitHub Discussions

### Phase 2: Convert (Months 6-12)
- Launch Pro tier
- Add cloud API proxy + AI post-processing
- Target: 5,000 MAU, 130 paying
- Marketing: Product Hunt launch, Reddit r/productivity, dev communities

### Phase 3: Expand (Months 12-24)
- Local Whisper mode (the big differentiator)
- VS Code extension (developer audience)
- Plugin system foundations
- Target: 15,000 MAU, 555 paying

### Phase 4: Scale (Months 24+)
- Team/Enterprise tier
- Plugin marketplace
- International expansion (localized marketing)
- Target: 50,000+ MAU

---

## 7. Why Open Source Works Here

| Objection | Counter-Evidence |
|-----------|-----------------|
| "OSS means you can't charge" | Obsidian: $25M ARR, 90%+ renewal. Users pay for trust, not lock-in |
| "You need VC funding" | Plausible, Vikunja: bootstrapped to sustainability |
| "Low conversion rates" | 3.7% of 15k MAU at $9/mo = $5k/mo. Achievable. |
| "Someone will fork and compete" | Forks lack hosted services, support, and momentum. Obsidian has many forks — none compete. |

### Contrarian Insight
> "Open source trust INCREASES willingness to pay for cloud services. Users pay because they trust you, not because they are locked in." — Pattern observed across Obsidian, Plausible, Ghost, Appwrite.

---

## 8. Revenue Diversification (Long-term)

| Stream | Timeline | Potential |
|--------|----------|-----------|
| Pro subscriptions | 6-12 months | Primary (70% of revenue) |
| Lifetime licenses | 6-12 months | Secondary (15%) |
| Team/Enterprise | 18-24 months | Growing (10%) |
| GitHub Sponsors | Immediate | Supplementary (5%) |
| Plugin marketplace | 24+ months | Future option |
| White-label licensing | 24+ months | Enterprise deals ($5k-50k/yr) |
