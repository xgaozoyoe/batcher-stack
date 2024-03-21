use crate::args::HashType;
use crate::args::OpenSchema;
use crate::batch::BatchInfo;
use crate::batch::CommitmentCheck;
use crate::batch::LastAggInfo;
use crate::proof::load_or_build_unsafe_params;
use crate::proof::ParamsCache;
use crate::proof::ProofInfo;
use crate::proof::ProofGenerationInfo;
use crate::proof::ProofPieceInfo;
use crate::proof::ProvingKeyCache;
use halo2_proofs::poly::commitment::ParamsVerifier;
use halo2aggregator_s::circuits::utils::store_instance;
use halo2aggregator_s::circuits::utils::TranscriptHash;
use halo2aggregator_s::solidity_verifier::codegen::solidity_aux_gen;
use halo2aggregator_s::solidity_verifier::solidity_render;

/*
use crate::profile::Profiler;
use crate::runtime::wasmi_interpreter::WasmRuntimeIO;
use crate::runtime::CompiledImage;
use crate::runtime::ExecutionResult;
use anyhow::Result;
use halo2_proofs::arithmetic::BaseExt;
use halo2_proofs::dev::MockProver;
*/
use halo2_proofs::pairing::bn256::Bn256;
use log::info;
use sha2::Digest;

use std::path::PathBuf;

pub fn generate_k_params(
    aggregate_k: u32,
    param_dir: &PathBuf,
    params_cache: &mut ParamsCache<Bn256>,
) {
    info!("Generating K Params file");

    // Setup Aggregate Circuit Params
    {
        let params_path = &param_dir.join(format!("K{}.params", aggregate_k));
        load_or_build_unsafe_params::<Bn256>(aggregate_k as usize, params_path, params_cache)
    };
}

pub fn exec_batch_proofs(
    params_cache: &mut ParamsCache<Bn256>,
    pkey_cache: &mut ProvingKeyCache<Bn256>,
    proof_name: &String,
    output_dir: &PathBuf,
    param_dir: &PathBuf,
    config_files: Vec<PathBuf>,
    commits: CommitmentCheck,
    hash: HashType,
    k: u32,
    cont: bool,
    use_ecc_select_chip: bool,
) {
    let mut target_k = None;
    let mut proofsinfo = vec![];
    let proofs = config_files
        .iter()
        .map(|config| {
            let proofloadinfo = ProofGenerationInfo::load(config);
            proofsinfo.push(proofloadinfo.clone());
            // target batch proof needs to use poseidon hash
            assert_eq!(proofloadinfo.hashtype, HashType::Poseidon);
            target_k = target_k.map_or(Some(proofloadinfo.k), |x| {
                // proofs in the same batch needs to have same size
                assert_eq!(x, proofloadinfo.k);
                Some(x)
            });
            ProofInfo::load_proof(&output_dir, &param_dir, &proofloadinfo)
        })
        .collect::<Vec<_>>()
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    let mut batchinfo = BatchInfo::<Bn256> {
        proofs,
        target_k: target_k.unwrap(),
        batch_k: k as usize,
        equivalents: vec![],
        absorb: vec![],
        expose: vec![],
    };

    batchinfo.load_commitments_check(&proofsinfo, commits);

    let is_final = hash == HashType::Sha || hash == HashType::Keccak;

    let param_file = format!("K{}.params", batchinfo.batch_k);

    let (last_proof_gen_info, agg_instances, shadow_instances, _)  = if cont {
            assert!(batchinfo.proofs.len() >= 3);
            let proof_piece = ProofPieceInfo::new("aggregate".to_string(), 0, batchinfo.get_agg_instance_size() as u32);
            // first round
            let mut proof_generation_info = ProofGenerationInfo::new(
                format!("{}.0", proof_name).as_str(),
                batchinfo.batch_k as usize,
                HashType::Poseidon
            );

            let (mut agg_proof_piece, mut instances, mut shadow_instances, mut last_hash) = batchinfo.batch_proof(
                proof_piece,
                &param_dir.clone(),
                &output_dir.clone(),
                params_cache,
                pkey_cache,
                false,
                true,
                proof_generation_info.hashtype,
                None // no previous agg
                );

            proof_generation_info.append_single_proof(agg_proof_piece);
            proof_generation_info.save(output_dir);

            proof_generation_info = ProofGenerationInfo::new(
                format!("{}.1", proof_name).as_str(),
                batchinfo.batch_k as usize,
                HashType::Poseidon
            );

            for _ in 1..batchinfo.proofs.len()-1 {
                let proof_piece = ProofPieceInfo::new("aggregate".to_string(), 1, batchinfo.get_agg_instance_size() as u32);
                (agg_proof_piece, instances, _, last_hash) = batchinfo.batch_proof(
                    proof_piece,
                    &param_dir.clone(),
                    &output_dir.clone(),
                    params_cache,
                    pkey_cache,
                    false,
                    true,
                    proof_generation_info.hashtype,
                    Some(vec![(1, 0, last_hash)]),
                    );

                proof_generation_info.append_single_proof(agg_proof_piece);
                proof_generation_info.save(output_dir);

                proof_generation_info = ProofGenerationInfo::new(
                    format!("{}.1", proof_name).as_str(),
                    batchinfo.batch_k as usize,
                    HashType::Poseidon
                    );
            }
            
            // Last round
            let proof_piece = ProofPieceInfo::new("aggregate".to_string(), 2, batchinfo.get_agg_instance_size() as u32);
            (agg_proof_piece, instances, shadow_instances, last_hash) = batchinfo.batch_proof(
                proof_piece,
                &param_dir.clone(),
                &output_dir.clone(),
                params_cache,
                pkey_cache,
                false,
                true,
                proof_generation_info.hashtype,
                Some(vec![(1, 0, last_hash)]),
                );

            proof_generation_info.append_single_proof(agg_proof_piece);
            proof_generation_info.save(output_dir);

        (proof_generation_info, instances, shadow_instances, last_hash)
    } else {
        // Singleton batch
        let mut proof_generation_info = ProofGenerationInfo::new(
            format!("{}", proof_name).as_str(),
            batchinfo.batch_k as usize,
            hash,
        );

        let proof_piece = ProofPieceInfo::new("aggregate".to_string(), 0, batchinfo.get_agg_instance_size() as u32);
        let (agg_proof_piece, instances, shadow_instances, last_hash) = batchinfo.batch_proof(
            proof_piece,
            &param_dir.clone(),
            &output_dir.clone(),
            params_cache,
            pkey_cache,
            is_final,
            false,
            hash,
            None
        );
        proof_generation_info.append_single_proof(agg_proof_piece);
        proof_generation_info.save(output_dir);
        (proof_generation_info, instances, shadow_instances, last_hash)
    };

    if hash == HashType::Sha || hash == HashType::Keccak {
        let proof: Vec<ProofInfo<Bn256>> =
            ProofInfo::load_proof(&output_dir, &param_dir, &last_proof_gen_info);

        info!("generate aux data for proof: {:?}", last_proof_gen_info);

        // setup batch params
        let params = load_or_build_unsafe_params::<Bn256>(
            last_proof_gen_info.k as usize,
            &param_dir.join(param_file),
            params_cache,
            );

        // the final instance size is 1
        let params_verifier: ParamsVerifier<Bn256> = params.verifier(1).unwrap();

        // generate solidity aux data
        // it only makes sense if the transcript challenge is poseidon
        match hash {
            HashType::Sha => {
                solidity_aux_gen::<_, sha2::Sha256>(
                    &params_verifier,
                    &proof[0].vkey,
                    &proof[0].instances[0],
                    proof[0].transcripts.clone(),
                    &output_dir.join(format!("{}.{}.aux.data", &last_proof_gen_info.name.clone(), 0)),
                    );
                store_instance(&vec![shadow_instances], &output_dir.join(format!("{}.{}.shadowinstance.data", &last_proof_gen_info.name.clone(), 0)))
            }
            HashType::Keccak => {
                solidity_aux_gen::<_, sha3::Keccak256>(
                    &params_verifier,
                    &proof[0].vkey,
                    &proof[0].instances[0],
                    proof[0].transcripts.clone(),
                    &output_dir.join(format!("{}.{}.aux.data", &last_proof_gen_info.name.clone(), 0)),
                    );
                store_instance(&vec![shadow_instances], &output_dir.join(format!("{}.{}.shadowinstance.data", &last_proof_gen_info.name.clone(), 0)))
            },
            HashType::Poseidon => unreachable!()
        }
    }
}

pub fn exec_solidity_gen<D: Digest + Clone>(
    param_dir: &PathBuf,
    output_dir: &PathBuf,
    k: u32,
    n_proofs: usize,
    sol_path_in: &PathBuf,
    sol_path_out: &PathBuf,
    aggregate_proof_info: &ProofGenerationInfo,
    params_cache: &mut ParamsCache<Bn256>,
    hasher: TranscriptHash,
) {
    let proof_params = load_or_build_unsafe_params::<Bn256>(
        k as usize,
        &param_dir.join(format!("K{}.params", k)),
        params_cache,
    );

    let proof_params_verifier: ParamsVerifier<Bn256> =
        proof_params.verifier(1).unwrap();

    println!("nproof {}", n_proofs);

    let proof: Vec<ProofInfo<Bn256>> =
        ProofInfo::load_proof(&output_dir, &param_dir, aggregate_proof_info);

    solidity_render::<_, D>(
        &(sol_path_in.to_str().unwrap().to_owned() + "/*"),
        sol_path_out.to_str().unwrap(),
        vec![(
            "AggregatorConfig.sol.tera".to_owned(),
            "AggregatorConfig.sol".to_owned(),
        )],
        "AggregatorVerifierStepStart.sol.tera",
        "AggregatorVerifierStepEnd.sol.tera",
        |i| format!("AggregatorVerifierStep{}.sol", i + 1),
        hasher,
        &proof_params_verifier,
        &proof[0].vkey,
        &proof[0].instances[0],
        proof[0].transcripts.clone(),
    );
}
