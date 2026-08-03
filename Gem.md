# Gem

|  |  |
| --- | --- |
| Owner | Katie |
| Doc Status | Initial Draft |
| Approved by Dev | To review |
| Approved for Public | Not Published |

## 1 Abstract

Gems is an incentive mechanism that enables businesses to create and distribute customer rewards derived from Intex. A merchant purchases an Intex, pays a premium, and parks it in the Gem Factory. The parked Intex becomes non-tradable, exempt from Call Events, and operates exclusively on the Outbe network. From then on, Gems are issued directly to customers at the moment of purchase, one Gem per purchase, with size proportional to the purchase amount — there is no pre-minted batch and no planned split. Each Gem receives its Entry Price and Floor Price based on the coen price at the moment of issuance. Gems inherit the fundamental settlement mechanics of Intex but with updated parameters, creating a flexible loyalty and reward system that bridges traditional merchant incentives with blockchain-based value creation.

## 2 Glossary

| TERM | DEFINITION |
| --- | --- |
| Gem | A non-fungible token derived from Intex, representing a conditional right to mine Promis, issued by merchants to customers at the moment of purchase. |
| Gem Factory | Protocol module/dApp that holds a parked Intex and issues Gems to customers on demand at the moment of purchase. |
| Merchant Account | Verified account authorized to operate a Gem Factory and issue Gems. |
| Customer Account | End-user account eligible to receive Gems from a merchant's Gem Factory. |
| Entry Price | The coen market price in the Issuance Currency at issuance, used as the basis for Cost Amount and Floor Price calculations. For Merchant Gems, must be ≥ source Intex Entry Price. |
| Cost Amount | Total cost basis of the Gem: Entry Price × Gem Load. The amount the holder pays at settlement. |
| Floor Price | Coen price threshold (in Reference Currency) required for settlement. Computed as 1.08 × Entry Price (in Reference Currency); for Merchant Gems must additionally be ≥ source Intex Floor Price. |
| Issuance Currency | The currency in which Entry Price and Cost Amount are denominated. Selected at issuance from the Issuance Currency list. |
| Reference Currency | The currency in which Floor Price is denominated. Selected at issuance from the Reference Currency short list (USD, EUR, GBP, CNY, JPY, HKD). |
| Settlement Currency | The Stable Token currency in which Cost Amount is paid on-chain. Equals Issuance Currency if a Stable Token / coen pair exists for it, otherwise equals Reference Currency. |
| Gem Call Threshold | Coen price level that triggers Call Event for Gems, 128% above the base price . |
| Gem Load | Quantity of Promis loaded into each individual Gem, derived from the source Intex. |

## 3 System Logic

### 3.1 Gem for Agents

Agents of the protocol receive their network coen allocation through Gems (after Genesis; for the Genesis period see Genesis spec, §3.1.2 below). An agent receives a Gem with a corresponding quantity of loaded coen. To realize the reward, the agent mines Promis from the Gem. The Gem is issued directly by the protocol.

The protocol issues a Gem to the allocation agent as the network reward, with a quantity of loaded coen (Gem Load) corresponding to the agent's allocation.

The Gem is minted with the following parameters:

- **Entry Price:** Coen market price in the Issuance Currency at the time of issuance, read from Price Oracle.
- **Cost Amount:** Entry Price × Gem Load. The amount the agent must pay at settlement to mine Promis from the Gem. **SRA coefficient:** For the SRA, a coefficient of 0.64 is applied to Cost Amount. This is an additional fine-tuning element of the allocation — the reduced Cost Amount accounts for the open banking fees the SRA must pay.
- **Floor Price:** 1.08 × coen market price in the Reference Currency at the time of issuance, read from Price Oracle.
- **Issuance Currency / Reference Currency:** Set by the protocol at issuance; default USD / USD.
- **Gem Load:** Quantity of Promis available to mine, corresponding to the loaded coen.
- **Call Rate:** 128 % — the standard Intex Call Rate. Agent Gems are subject to Call Events on the same terms as base Intex.
- **Call Threshold:** Entry Price × (1 + Call Rate), denominated in the Reference Currency. Coen price level whose breach triggers a Call Event for the Gem.

Once the Floor Price condition is met, the agent can pay the Cost Amount and mine Promis from the Gem.

The Gem is burned on mining; mined Promis follow the standard Promis lifecycle and can be used to mine coens.

If coen price exceeds the Call Threshold on at least 20 out of 30 consecutive days while the Gem is still unsettled, a **Call Event** is triggered: the agent has 8 days (Call Notice Period) to pay Cost Amount and mine Promis. If the deadline is missed, the Gem is burned with reason `call_default` and the Promis entitlement is forfeited. The Call Event applies regardless of status.

#### 3.1.1 Agent Gem Lifecycle

##### State Flow

```javascript
[1] `Issued State` (issued by protocol to agent)
    │
    │  Floor Price condition met
    ▼
[2] `Qualified State`
    │
    ├── Agent pays Cost Amount
    │       └──▶ [3] `Settled State` ──`Promis Mined` (event)──▶ [5] `Burned` (event)
    │
    └── Call Event triggered
            └──▶ [4] `Called State`
                  ├── Settled in time
                  │       └──▶ [3] `Settled State` ──`Promis Mined` (event)──▶ [5] `Burned` (event)
                  └── Missed deadline ──`Forfeited` (event)──▶ [5] `Burned` (event)

```

Note: A Call Event can also be triggered while the Gem is still in the Issued state. In that case the Gem transitions directly from Issued to Called, and the agent must settle within the 8-day Call Notice Period — Floor Price conditions do not block Call settlement.

##### State Descriptions

| STATE | TRIGGER | DESCRIPTION |
| --- | --- | --- |
| Issued | Protocol issuance to agent | Gem is issued directly by the protocol as the agent's network reward. Settlement is not yet possible — Floor Price condition must first be met. |
| Qualified | Floor Price condition met (current coen price strictly above Floor Price in Reference Currency) | Agent can settle at any time by paying Cost Amount. |
| Called | Call Event triggered (coen price exceeded Call Threshold for 20 of 30 consecutive days) | Agent must settle within the 8-day Call Notice Period regardless of status. Applies from Issued or Qualified state. |
| Settled | Cost Amount paid | Agent has paid Cost Amount to Reserve, eligible for Promis mining. |

#### 3.1.2 Gem for Validators in genesis phase

See Genesis spec.

### 3.2 Gem for Merchants

#### 3.2.1 Intex Acquisition and Conversion

##### 3.2.1.1 Merchant Intex Purchase

- Merchant acquires Intex through standard IBA channels or secondary market.
- Merchant pays Premium for the Intex.
- Intex must be in Tradable state for conversion (Call Event hasn't occurred yet).
- Merchant initiates conversion request via Gem Factory.

##### 3.2.1.2 Conversion Process

- Gem Factory receives the source Intex.
- Source Intex transitions to Frozen State:
    - Non-tradable — transfers blocked permanently.
    - Call-exempt — no Call Events triggered regardless of coen price.
    - Outbe-only — operates exclusively on Outbe network.
- The source Intex is **parked whole** in the Factory as a pool of Promis capacity. Gems are **not** pre-minted and **not**pre-split into planned fractions at this stage.
- During the Factory's active period, Gems are issued on demand, one per customer purchase, with size proportional to that purchase (see §3.2.2). The source Intex is gradually drained as Gems are issued.

#### 3.2.2 Gem Factory Operations

##### 3.2.2.1 Gem Issuance on Purchase

Gems are issued **on demand**, at the moment of a customer purchase. There is no pre-production batch. The Factory acts as a pool of Promis capacity (the parked source Intex) from which Gems are drawn one purchase at a time.

- When a customer makes a purchase at the merchant, the Factory issues a single Gem directly to that customer.
- **Gem Load (per-purchase Promis load)** is proportional to the size of that specific purchase, computed against the source Intex according to the merchant's Factory configuration. Different customers thus receive Gems with different Loads depending on how much they spent.
- The source Intex's remaining Promis capacity decreases by the Gem Load on each issuance.
- Mint and Distribute are a single atomic step — there is no intermediate state where a Gem exists in the Factory but is not yet owned by a customer.

##### 3.2.2.2 Gem Configuration

The merchant configures the Factory's parameters once at Factory setup (i.e. at Intex parking). These parameters apply to every Gem issued from this Factory:

- **Issuance Currency / Reference Currency:** Merchant selects both at Factory setup, independently of the source Intex (they are **not** inherited from the source Intex). Issuance Currency is chosen from the Issuance Currency list; Reference Currency is chosen from the Reference Currency short list.
- **Gem Call Rate:** 128%

The following parameters are computed at the moment each Gem is issued (per purchase), not at Factory setup:

- **Gem Load:** Determined on the fly from the size of the customer's purchase against the source Intex (see §3.2.2.1).
- **Entry Price:** Must be ≥ max(current coen market price in the Issuance Currency, source Intex Entry Price). If coen price has increased since Intex issuance, Entry Price is set higher; if coen price is unchanged or decreased, Entry Price remains at the source Intex level.
- **Cost Amount:** Entry Price × Gem Load. The customer pays this in full at settlement.
- **Floor Price:** max(1.08 × Entry Price, source Intex Floor Price), denominated in the Reference Currency. The 1.08 markup is applied to the Entry Price converted into the Reference Currency at the issuance-time oracle rate.
- **Call Threshold:** Derived from Entry Price and the Factory's configured Gem Call Rate.

#### 3.2.3 Gem Distribution

Distribution is not a separate step. Each Gem is issued directly to the customer at the moment of their purchase (see §3.2.2.1) — mint and distribute happen atomically.

- Gems can only be issued to verified Customer Accounts.
- Each Gem is a distinct NFT with a unique identifier and is owned by the receiving customer from the moment of issuance.
- ?? No transfers between customers — Gems are non-tradable.

#### 3.2.4 Gem Lifecycle

##### 3.2.4.1 State Flow

```javascript
[1] `Issued State` (issued directly to customer at purchase)
    │
    │  Floor Price condition met
    ▼
[2] `Qualified State`
    │
    ├── Customer pays Cost Amount
    │       └──▶ [3] `Settled State` ──`Promis Mined` (event)──▶ [5] `Burned` (event)
    │
    └── Call Event triggered
            └──▶ [4] `Called State`
                  ├── Settled in time
                  │       └──▶ [3] `Settled State` ──`Promis Mined` (event)──▶ [5] `Burned` (event)
                  └── Missed deadline ──`Forfeited` (event)──▶ [5] `Burned` (event)

```

##### 3.2.4.2 State Descriptions

| STATE | TRIGGER | DESCRIPTION |
| --- | --- | --- |
| Issued | Customer purchase | Gem is issued directly to the customer at the moment of purchase. Settlement is not yet possible —  Floor Price condition must first be met. |
| Qualified | Floor Price condition met (current coen price strictly above Floor Price in Reference Currency) | Customer can settle at any time by paying Cost Amount. |
| Called | Call Event triggered | Coen price exceeded Call Threshold for 20 out of 30 consecutive days. Customer must settle within 8-day Call Notice Period. |
| Settled | Cost Amount paid | Customer has paid Cost Amount to Reserve, eligible for Promis mining. |

#### 3.2.5 Gem Settlement

##### 3.2.5.1 Settlement Process

A Gem can be settled only after it has transitioned to Qualified state (Floor Price condition is met).

- Customer settles Gem by paying Cost Amount in the Settlement Currency to Reserve. (For Genesis Gems, Cost Amount is 0, so no payment is required.)
- **Settlement Currency selection:**
    - If the Issuance Currency has a corresponding Stable Token on the Network → Settlement Currency = Issuance Currency.
    - If the Issuance Currency does not have a corresponding Stable Token → Settlement Currency = Reference Currency (which by definition has a Stable Token / coen pair).
- Settlement is performed at the spot exchange rate of the Issuance Currency against the Settlement Currency at the moment of settlement (via oracle), so the amount paid corresponds to the Cost Amount denominated in the Issuance Currency.
- Upon successful payment, Gem transitions to Settled State.
- Customer can mine Promis at any time after settlement.

#### 3.2.6 Call Event

##### 3.2.6.1 Call Trigger

A Call Event is triggered if the coen price exceeds the Call Threshold on at least 20 out of 30 consecutive days. The Call Threshold is calculated based on the Gem's parameters.

##### 3.2.6.2 Call Notice Period

- When a Call Event is triggered, the customer has 8 days (Call Notice Period) to settle.
- Customer must pay Cost Amount within this period.
- If the deadline is missed, the Gem transitions to Burned State with reason `call_default`.
- A burned Gem cannot be recovered — Promis entitlement is forfeited.

#### 3.2.7 Promis Mining from Gem

- After settlement, the customer initiates Mining Module via Wallet App.
- Gem is burned.
- Promis quantity equal to Gem Load is mined to customer's Account.
- Promis can then be used to mine coens (standard Promis lifecycle).

#### 3.2.8 Source Intex Handling

##### 3.2.8.1 Frozen Intex Rules

Once Intex is parked in the Factory:

- **Non-Tradable:** Cannot be transferred to any other account.
- **Network Restriction:** Operates only on Outbe network (no cross-chain).
- **Settlement Blocked:** Cannot be settled directly — only through Gem issuance.
- **Validity Period:** The source Intex remains valid for **1 year from&#160;`parked_at`**. After this period the source Intex **expires**: no new Gems can be issued from the Factory and any `remaining_capacity` (undrained Promis) is forfeited. Gems already issued before expiration are unaffected and continue through their normal lifecycle (Issued → Qualified / Called → Settled → Burned).

## 4 Attributes

### 4.1 Gem Factory Module Attributes

| ATTRIBUTE | TYPE | DESCRIPTION | EXAMPLE |
| --- | --- | --- | --- |
| `total_gems_issued` | integer [10^18] | Total Gems issued (to agents, customers, or as Genesis Gems) | 1,500,000 |
| `total_intex_parked` | integer [10^18] | Total Intex parked in the module | 1,500 |

### 4.2 Gem Factory Record Attributes

A Factory record is created when a merchant parks an Intex (no batch of pre-minted Gems is produced). Per-Gem values such as `entry_price`, `cost_amount`, `gem_load`, and `call_threshold` are computed on the fly at each purchase and recorded on the individual Gem (see §4.3).

| ATTRIBUTE | TYPE | DESCRIPTION | EXAMPLE |
| --- | --- | --- | --- |
| `factory_id` | string | Unique Factory identifier | "FACT-2025-001" |
| `merchant_address` | address | Merchant's Outbe wallet address | 0x1234...abcd |
| `source_intex_id` | string | ID of parked Intex | "INT-20250701-A" |
| `remaining_capacity` | integer [10^18] | Promis capacity still available in the source Intex (decreases as Gems are issued) | 850,000 |
| `issuance_currency` | integer (ISO 4217) | Issuance Currency code (Factory-level setting, applied to every Gem) | 840 (USD) |
| `reference_currency` | integer (ISO 4217) | Reference Currency code (Factory-level setting, applied to every Gem) | 840 (USD) |
| `gem_call_rate` | double | Call rate applied to every Gem from this Factory (1.28) | 0.96 |
| `parked_at` | timestamp | When the source Intex was parked in the Factory | 2025-07-15T10:00:00Z |

### 4.3 Gem Attributes

| ATTRIBUTE | TYPE | DESCRIPTION | EXAMPLE |
| --- | --- | --- | --- |
| `gem_id` | string | Unique Gem identifier | "GEM-2025-0042" |
| `factory_id` | string | Factory that issued this Gem (null for protocol-issued Gems — Agent, Genesis) | "FACT-2025-001" |
| `owner` | address | Customer's / agent's wallet address (set at issuance — there is no undistributed state for Merchant Gems) | 0x5678...efgh |
| `gem_load` | integer [10^18] | Promis quantity loaded. For Merchant Gems, determined by the customer's purchase size at issuance. | 1,000 |
| `entry_price` | integer [10^18] | Coen market price in Issuance Currency at issuance | 6.50 USDT |
| `cost_amount` | integer [10^18] | Total cost basis: entry\_price × gem\_load (in Issuance Currency). Amount the holder pays at settlement (0 for Genesis Gems). For SRA Agent Gems, multiplied by 0.64. | 6,500 USDT |
| `floor_price` | integer [10^18] | Floor Price in Reference Currency. For Merchant Gems: max(1.08 × entry\_price in Reference Currency, source Intex floor\_price). For Agent Gems: 1.08 × coen market price in Reference Currency. | $7.02 |
| `issuance_currency` | integer (ISO 4217) | Issuance Currency code | 840 (USD) |
| `reference_currency` | integer (ISO 4217) | Reference Currency code | 840 (USD) |
| `call_threshold` | integer [10^18] | Coen price triggering Call Event | $12.74 |
| `state` | enum | Current Gem state | "ISSUED", "QUALIFIED", "CALLED", "SETTLED" |
| `issued_at` | timestamp | When Gem was issued to customer | 2025-07-20T14:30:00Z |
| `qualified_at` | timestamp | When Gem transitioned to Qualified (Floor Price condition met) | 2025-08-10T14:30:00Z |
| `called_at` | timestamp | When Call Event was triggered (if applicable) | null |
| `settled_at` | timestamp | When Gem was settled (if applicable) | null |

### 4.4 Source Intex (Frozen) Attributes

| ATTRIBUTE | TYPE | DESCRIPTION | EXAMPLE |
| --- | --- | --- | --- |
| `intex_id` | string | Original Intex ID | "INT-20250701-A" |
| `factory_id` | string | Linked Factory ID | "FACT-2025-001" |
| `frozen_state` | enum | Set to "FROZEN" on conversion; transitions to "EXPIRED" 1 year after `frozen_at` | "FROZEN", "EXPIRED" |
| `original_entry_price` | integer [10^18] | Entry Price at time of Intex issuance | 6,000 USDT |
| `chain_id` | string | Always "OUTBE" after conversion | "OUTBE" |
| `frozen_at` | timestamp | When Intex was parked in the Factory | 2025-07-15T10:00:00Z |
| `expires_at` | timestamp | When the source Intex expires (`frozen_at` + 1 year). After this point no new Gems can be issued and any remaining Promis capacity is forfeited. | 2026-07-15T10:00:00Z |

## 5 Commands

| SC COMMAND | ARGUMENTS | DESCRIPTION |
| --- | --- | --- |
| `setup_factory()` | `address merchant`, `string intex_id`, `uint16 issuance_currency`, `uint16 reference_currency`, `double gem_call_rate` | Park an Intex in the Gem Factory and configure Factory-level parameters. No Gems are minted at this step. |
| `issue_gem()` | `string factory_id`, `address customer`, `uint256 gem_load` | Issue a single Gem to a customer at the moment of purchase. `gem_load` is proportional to the purchase size. Mint and distribute are atomic. |
| `settle_gem()` | `string gem_id` | Customer settles Gem by paying Cost Amount in the Settlement Currency. |
| `mine_gem_promis()` | `string gem_id` | Mine Promis from settled Gem. |
| `get_gem_status()` | `string gem_id` | Query Gem details and state. |
| `list_factory_gems()` | `string factory_id` | List all Gems issued by a Factory. |
| `list_merchant_gems()` | `address merchant` | List all Gems issued by merchant. |
| `list_customer_gems()` | `address customer` | List all Gems owned by customer. |
| `check_call_status()` | `string gem_id` | Check if Gem is approaching or in Call Event. |
| `get_module_stats()` | — | Query overall Gem Factory module statistics. |

## 6 Open Questions

- **Customer Verification:** What verification requirements apply to Customer Accounts receiving Gems?
- **Gem Transferability:** Should there be any circumstances under which Gems can be transferred between customers?
- **Partial Settlement:** Can a Gem be partially settled, or must the full Cost Amount be paid?
- **Cross-Merchant Gems:** Can customers accumulate Gems from multiple merchants in a unified view?

## Appendix A: Comparison with Standard Intex

| FEATURE | STANDARD INTEX | GEM |
| --- | --- | --- |
| Issuer | Network (via Lysis) | Gem Factory (from Intex) |
| Recipient | IBA / Market Participant | Customer |
| Tradable | Yes (in Tradable state) | No |
| Call Event | Yes (20/30 day rule, 64% rate) | Yes (20/30 day rule, 64–128% rate) |
| Entry Price | Fixed at issuance | ≥ max(current coen market price in Issuance Currency, source Intex Entry Price) |
| Floor Price | Set at issuance | max(1.08 × Entry Price in Reference Currency, source Intex Floor Price) |
| Call Rate | Fixed 64% | Gem Call Rate (64–128%) |
| Network | Multi-chain | Outbe only |
| Settlement | Owner pays Cost Amount in Settlement Currency | Customer pays Cost Amount in Settlement Currency |
| Premium | Intex Price (8% of Cost) | None |
