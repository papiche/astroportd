//! Construction hors ligne : appels de virement et extrinsics signés sans nœud.

use subxt::utils::{AccountId32, MultiAddress};

use crate::{Account, Amount, ChainError, TransferMode};

/// Lot `utility.batch([virement, system.remark_with_event(mémo)])` : le mémo voyage dans la même transaction.
pub fn memo_batch(dest: MultiAddress<AccountId32, ()>, amount: Amount, mode: TransferMode, memo: &str) -> impl subxt::tx::Payload {
    use astro_g1_runtime::runtime;
    use astro_g1_runtime::runtime::runtime_types::pallet_balances::pallet::Call as BalancesCall;
    use astro_g1_runtime::runtime::runtime_types::frame_system::pallet::Call as SystemCall;
    use astro_g1_runtime::runtime::runtime_types::g1_runtime::RuntimeCall;
    let transfer = match mode {
        TransferMode::AllowDeath => BalancesCall::transfer_allow_death { dest, value: amount.0 },
        TransferMode::KeepAlive => BalancesCall::transfer_keep_alive { dest, value: amount.0 },
    };
    runtime::tx().utility().batch(vec![RuntimeCall::Balances(transfer), RuntimeCall::System(SystemCall::remark_with_event { remark: memo.as_bytes().to_vec() })])
}


/// Paramètres de chaîne nécessaires à une signature hors ligne.
#[derive(Debug, Clone, Copy)]
pub struct ChainInfo {
    /// Hash du bloc de genèse.
    pub genesis_hash: [u8; 32],
    /// Version du runtime.
    pub spec_version: u32,
    /// Version des transactions.
    pub transaction_version: u32,
}

/// Metadata embarquée du runtime Ğ1.
pub fn metadata() -> Result<subxt::Metadata, ChainError> {
    <subxt::Metadata as parity_scale_codec::Decode>::decode(&mut &astro_g1_runtime::METADATA[..]).map_err(|e| ChainError::Invalid(format!("metadata illisible : {e}")))
}

/// Extrinsic de virement signé hors ligne (aucun nœud nécessaire) : octets prêts à être soumis.
pub fn sign_transfer_offline(info: &ChainInfo, signer: &impl subxt::tx::Signer<subxt::config::PolkadotConfig>, nonce: u64, dest: &Account, amount: Amount, mode: TransferMode, memo: Option<&str>) -> Result<Vec<u8>, ChainError> {
    use astro_g1_runtime::runtime;
    use subxt::client::RuntimeVersion;
    use subxt::client::OfflineClient;
    use subxt::config::{DefaultExtrinsicParamsBuilder, PolkadotConfig};
    let client = OfflineClient::<PolkadotConfig>::new(
        subxt::utils::H256(info.genesis_hash),
        RuntimeVersion { spec_version: info.spec_version, transaction_version: info.transaction_version },
        metadata()?,
    );
    let params = DefaultExtrinsicParamsBuilder::<PolkadotConfig>::new().nonce(nonce).build();
    let to = MultiAddress::Id(dest.account_id());
        let tx = client.tx();
    let err = |e: subxt::Error| ChainError::Invalid(e.to_string());
    let signed = match (memo, mode) {
        (None, TransferMode::AllowDeath) => tx.create_partial_offline(&runtime::tx().balances().transfer_allow_death(to, amount.0), params).map(|mut p| p.sign(signer)),
        (None, TransferMode::KeepAlive) => tx.create_partial_offline(&runtime::tx().balances().transfer_keep_alive(to, amount.0), params).map(|mut p| p.sign(signer)),
        (Some(m), mode) => tx.create_partial_offline(&memo_batch(to, amount, mode, m), params).map(|mut p| p.sign(signer)),
    }
    .map_err(err)?;
    Ok(signed.encoded().to_vec())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    use subxt::config::PolkadotConfig;
    use subxt::tx::Signer;
    use subxt::utils::MultiSignature;

    use super::*;
    use crate::Ed25519Signer;

    const INFO: ChainInfo = ChainInfo { genesis_hash: [7; 32], spec_version: 1000, transaction_version: 1 };

    /// Signataire qui garde ce qu'on lui a fait signer, pour vérifier la signature indépendamment.
    struct Recording {
        inner: Ed25519Signer,
        seen: Mutex<Vec<(Vec<u8>, [u8; 64])>>,
    }

    impl Signer<PolkadotConfig> for Recording {
        fn account_id(&self) -> subxt::utils::AccountId32 {
            self.inner.account().account_id()
        }

        fn sign(&self, payload: &[u8]) -> MultiSignature {
            let sig = self.inner.sign_bytes(payload);
            self.seen.lock().unwrap().push((payload.to_vec(), sig));
            MultiSignature::Ed25519(sig)
        }
    }

    fn signer() -> Recording {
        Recording { inner: Ed25519Signer::from_seed([3; 32]), seen: Mutex::new(Vec::new()) }
    }

    #[test]
    fn metadata_is_the_g1_runtime_with_the_calls_we_use() {
        let m = metadata().unwrap();
        for (pallet, call) in [("Balances", "transfer_allow_death"), ("Balances", "transfer_keep_alive"), ("Utility", "batch"), ("System", "remark_with_event")] {
            assert!(m.pallet_by_name(pallet).and_then(|p| p.call_variant_by_name(call)).is_some(), "{pallet}::{call}");
        }
    }

    #[test]
    fn a_transfer_is_signed_in_memory_as_ed25519_and_the_signature_verifies() {
        let s = signer();
        let dest = Account([9; 32]);
        let bytes = sign_transfer_offline(&INFO, &s, 5, &dest, Amount(1250), TransferMode::KeepAlive, None).unwrap();
        // Extrinsic v4 signé : [longueur compacte] 0x84, adresse Id (0x00 + clé), signature Ed25519 (0x00 + 64 o).
        let body_start = if bytes[0] & 0b11 == 0b01 { 2 } else { 1 };
        assert_eq!(bytes[body_start], 0x84, "extrinsic signé version 4");
        assert_eq!(bytes[body_start + 1], 0x00, "MultiAddress::Id");
        assert_eq!(&bytes[body_start + 2..body_start + 34], &s.inner.account().0);
        assert_eq!(bytes[body_start + 34], 0x00, "MultiSignature::Ed25519");
        let sig = &bytes[body_start + 35..body_start + 99];
        let seen = s.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(sig, &seen[0].1);
        VerifyingKey::from_bytes(&s.inner.account().0).unwrap().verify(&seen[0].0, &Signature::from_bytes(&seen[0].1)).unwrap();
        assert!(bytes.windows(32).any(|w| w == dest.0), "le destinataire figure dans l'appel");
    }

    #[test]
    fn nonce_modes_and_memo_change_the_extrinsic() {
        let s = signer();
        let dest = Account([9; 32]);
        let base = sign_transfer_offline(&INFO, &s, 1, &dest, Amount(100), TransferMode::AllowDeath, None).unwrap();
        assert_eq!(base, sign_transfer_offline(&INFO, &s, 1, &dest, Amount(100), TransferMode::AllowDeath, None).unwrap(), "ed25519 est déterministe");
        assert_ne!(base, sign_transfer_offline(&INFO, &s, 2, &dest, Amount(100), TransferMode::AllowDeath, None).unwrap());
        assert_ne!(base, sign_transfer_offline(&INFO, &s, 1, &dest, Amount(101), TransferMode::AllowDeath, None).unwrap());
        assert_ne!(base, sign_transfer_offline(&INFO, &s, 1, &dest, Amount(100), TransferMode::KeepAlive, None).unwrap());
        let memo = "UPLANET:g1LBF94v:ZEN:abcdef12->12345678";
        let with_memo = sign_transfer_offline(&INFO, &s, 1, &dest, Amount(100), TransferMode::AllowDeath, Some(memo)).unwrap();
        assert!(with_memo.windows(memo.len()).any(|w| w == memo.as_bytes()), "le mémo voyage dans la même transaction (utility.batch)");
        assert!(!base.windows(8).any(|w| w == b"UPLANET:"));
        let other_chain = ChainInfo { genesis_hash: [8; 32], ..INFO };
        assert_ne!(base[base.len() - 80..], sign_transfer_offline(&other_chain, &s, 1, &dest, Amount(100), TransferMode::AllowDeath, None).unwrap()[base.len() - 80..], "la genèse est signée : pas de rejeu entre chaînes");
    }
}
