use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer, Burn, MintTo, burn, transfer, mint_to};
use std::mem::size_of;

declare_id!("D7dQejiULpCMewkTHH4nTYNEhSEMwWyYdczQ9HKqdsaE");

// Constants simulating the parameters used in the bonding curve (like A, B, Fee in Solidity).
const A: u128 = 1073000191;
const B: u128 = 32190005730;
const PROGRESS_THRESHOLD: u64 = 263300 * 1_000_000_000_000_000_000u64; // 263300 * 10^18
const PLATFORM_FEE_BP: u64 = 50; // 0.5% (basis points)
const DEX_FEE_BP: u64 = 50;      // 0.5% (basis points)

#[program]
pub mod boomerfun {
    use super::*;

    // Initialize the program state. This can only be called once.
    pub fn initialize(ctx: Context<InitializeProgram>) -> Result<()> {
        let state = &mut ctx.accounts.program_state;
        state.token_count = 1;
        state.total_fee_collected = 0;
        Ok(())
    }

    // Create a new Token and record its information
    pub fn create_token(
        ctx: Context<CreateToken>,
        name: String,
        symbol: String,
    ) -> Result<()> {
        let state = &mut ctx.accounts.program_state;

        // The process of transferring a token creation fee is omitted here; you can add it yourself
        // Example: state.total_fee_collected += 50 * 10^18 ... (pay attention to units in Solana)

        // Initialize a new TokenInfo
        let token_info = TokenInfo {
            mint: ctx.accounts.token_mint.key(),    // Store the mint address of this SPL token
            name,
            symbol,
            creator: *ctx.accounts.user.key,        // Record who called this function
            token_sold: 0,
            currency_collected: 0,
            is_dex_phase: false,
        };

        state.tokens.push(token_info);
        state.token_count += 1;

        Ok(())
    }

    // Purchase tokens
    pub fn purchase_token(ctx: Context<PurchaseToken>, token_id: u64, purchase_currency_amount: u64) -> Result<()> {
        let state = &mut ctx.accounts.program_state;
        require!(token_id < state.token_count, ErrorCode::InvalidTokenId);

        let token_info = &mut state.tokens[token_id as usize];
        if token_info.is_dex_phase {
            return err!(ErrorCode::AlreadyInDexPhase);
        }

        // For example: first transfer the user's currency into the contract's vault
        // Anchor has a built-in token transfer (user -> vault).
        // You need the user's currency_token_ata and the vault's currency_token_ata, etc.
        // The following is just an example.
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.user_currency_account.to_account_info(),
                    to: ctx.accounts.vault_currency_account.to_account_info(),
                    authority: ctx.accounts.user.to_account_info(),
                },
            ),
            purchase_currency_amount,
        )?;

        // Calculate the platform fee
        let fee = (purchase_currency_amount as u128 * PLATFORM_FEE_BP as u128) / 10000;
        // In Solana, be mindful of the u64 range.
        let fee_u64 = fee as u64;
        state.total_fee_collected += fee_u64;

        let net_funds = purchase_currency_amount - fee_u64;

        // Use the bonding curve to calculate how many tokens to give the user
        // This is a simplified example mimicking the Solidity formula
        let x1 = token_info.currency_collected as u128;
        let denominator = (30_000_000_000_000_000_000u128)
            .checked_add((x1 + net_funds as u128) / 3000)
            .unwrap();
        let y2 = A
            .checked_mul(1_000_000_000_000_000_000u128)
            .unwrap()
            .checked_sub(
                (B
                    .checked_mul(1_000_000_000_000_000_000_000_000_000_000u128)
                    .unwrap())
                .checked_div(denominator)
                .unwrap(),
            )
            .unwrap();
        let token_amount = y2.checked_sub(token_info.token_sold).unwrap();

        // token_amount is u128; be careful with overflow in Solana
        let token_amount_u64 = token_amount as u64;

        // Either mint or transfer from the contract's token balance to the user (depending on your design)
        // Example: from the vault -> user
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.vault_agent_token_account.to_account_info(),
                    to: ctx.accounts.user_agent_token_account.to_account_info(),
                    authority: ctx.accounts.vault_authority.to_account_info(),
                },
            ),
            token_amount_u64,
        )?;

        // Update the state
        token_info.currency_collected += net_funds;
        token_info.token_sold += token_amount;

        // Check if threshold is reached
        check_progress(token_info)?;

        Ok(())
    }

    // Sell tokens
    pub fn sell_token(ctx: Context<SellToken>, token_id: u64, sell_amount: u64) -> Result<()> {
        let state = &mut ctx.accounts.program_state;
        require!(token_id < state.token_count, ErrorCode::InvalidTokenId);

        let token_info = &mut state.tokens[token_id as usize];
        if token_info.is_dex_phase {
            return err!(ErrorCode::AlreadyInDexPhase);
        }
        require!(sell_amount > 0, ErrorCode::InvalidAmount);

        // First transfer the user's agent tokens to the contract for burning or freezing
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.user_agent_token_account.to_account_info(),
                    to: ctx.accounts.vault_agent_token_account.to_account_info(),
                    authority: ctx.accounts.user.to_account_info(),
                },
            ),
            sell_amount,
        )?;

        // Calculate how much currency to return to the user
        let x1 = token_info.currency_collected as u128;
        let y1 = token_info.token_sold;
        let y2 = y1.checked_sub(sell_amount as u128).unwrap();
        let x2 = (B
            .checked_mul(1_000_000_000_000_000_000_000_000_000_000u128)
            .unwrap())
            .checked_div(
                A
                    .checked_mul(1_000_000_000_000_000_000u128)
                    .unwrap()
                    .checked_sub(y2)
                    .unwrap(),
            )
            .unwrap()
            .checked_sub(30_000_000_000_000_000_000u128)
            .unwrap()
            .checked_mul(3000)
            .unwrap();

        let currency_to_pay = x1.checked_sub(x2).unwrap();
        let currency_to_pay_u64 = currency_to_pay as u64;

        // Calculate the fee
        let fee = (currency_to_pay_u64 as u128 * PLATFORM_FEE_BP as u128) / 10000;
        let fee_u64 = fee as u64;

        let net_currency_to_pay = currency_to_pay_u64 - fee_u64;
        state.total_fee_collected += fee_u64;

        // Update the token info
        token_info.currency_collected = x2 as u64;
        token_info.token_sold = y2;

        // Transfer the currency back to the user
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.vault_currency_account.to_account_info(),
                    to: ctx.accounts.user_currency_account.to_account_info(),
                    authority: ctx.accounts.vault_authority.to_account_info(),
                },
            ),
            net_currency_to_pay,
        )?;

        Ok(())
    }
}

// Auxiliary function
fn check_progress(token_info: &mut TokenInfo) -> Result<()> {
    if !token_info.is_dex_phase && token_info.currency_collected >= PROGRESS_THRESHOLD {
        token_info.is_dex_phase = true;
        // Trigger transferToDEX
        // Omitted here. You can integrate with Raydium or another DEX as needed.
    }
    Ok(())
}

// --------------------------------------------------
// Context & State
// --------------------------------------------------

#[derive(Accounts)]
pub struct InitializeProgram<'info> {
    // Initialize the ProgramState account. This can only be done once. 
    #[account(init, payer = user, space = 8 + size_of::<ProgramState>())]
    pub program_state: Account<'info, ProgramState>,
    #[account(mut)]
    pub user: Signer<'info>,
    /// System program
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CreateToken<'info> {
    #[account(mut)]
    pub program_state: Account<'info, ProgramState>,
    #[account(mut)]
    pub user: Signer<'info>,

    /// The following fields are examples, assuming token_mint is an already-created SPL Mint.
    /// If you want to initialize the mint within the contract, you would need additional parameters.
    #[account(mut)]
    pub token_mint: Account<'info, Mint>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    // You may also need the rent or sysvar accounts here.
}

#[derive(Accounts)]
pub struct PurchaseToken<'info> {
    #[account(mut)]
    pub program_state: Account<'info, ProgramState>,

    #[account(mut)]
    pub user: Signer<'info>,
    
    // The user's currency token account
    #[account(mut)]
    pub user_currency_account: Account<'info, TokenAccount>,
    // The vault's currency token account (owned by the contract)
    #[account(mut)]
    pub vault_currency_account: Account<'info, TokenAccount>,

    // The user's agent token account (the user receives tokens here)
    #[account(mut)]
    pub user_agent_token_account: Account<'info, TokenAccount>,
    // The vault's agent token account (holds token inventory)
    #[account(mut)]
    pub vault_agent_token_account: Account<'info, TokenAccount>,

    // Represents the program's authority (e.g. PDA)
    /// CHECK: Example only, no validation here
    #[account(mut)]
    pub vault_authority: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct SellToken<'info> {
    #[account(mut)]
    pub program_state: Account<'info, ProgramState>,
    #[account(mut)]
    pub user: Signer<'info>,

    #[account(mut)]
    pub user_agent_token_account: Account<'info, TokenAccount>,
    #[account(mut)]
    pub vault_agent_token_account: Account<'info, TokenAccount>,

    #[account(mut)]
    pub user_currency_account: Account<'info, TokenAccount>,
    #[account(mut)]
    pub vault_currency_account: Account<'info, TokenAccount>,

    /// CHECK: Example only, no validation here
    #[account(mut)]
    pub vault_authority: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
}

// Program state
#[account]
pub struct ProgramState {
    pub token_count: u64,        // Similar to tokenCount in Solidity
    pub total_fee_collected: u64,
    pub tokens: Vec<TokenInfo>,  // Stores information for all tokens
}

// Custom structure. We must manually implement Anchor serialization/deserialization.
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct TokenInfo {
    pub mint: Pubkey,
    pub name: String,
    pub symbol: String,
    pub creator: Pubkey,
    pub token_sold: u128,
    pub currency_collected: u64,
    pub is_dex_phase: bool,
}

// Custom error codes
#[error_code]
pub enum ErrorCode {
    #[msg("Invalid token ID")]
    InvalidTokenId,
    #[msg("Already in DEX phase")]
    AlreadyInDexPhase,
    #[msg("Invalid amount")]
    InvalidAmount,
}

