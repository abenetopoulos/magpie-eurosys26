use std::cmp::max;
use std::mem::size_of as mem_size_of;

use nando_support::{
    activation_intent::NandoArgument, allocate_and_init, iptr::IPtr, nando_spawn, nando_yield,
    register_initial,
};
use nandoize::nandoize_lib;
use object_tracker::object_tracker_tls;
use ownership_tracker::ownership_tracker_tls;

use crate::definitions::*;

pub mod definitions;
pub mod resolver;

pub(crate) const NAMESPACE: &'static str = "smith_waterman";

#[nandoize_lib]
pub fn process_block(
    algorithm_state: &State,
    mode: u8, // FIXME change to `Mode`
    horizontal_string_start: usize,
    horizontal_string_end: usize,
    vertical_string_start: usize,
    vertical_string_end: usize,
    right_halo_dependency: Option<&BlockResult>,
    bottom_halo_dependency: Option<&BlockResult>,
    bottom_right_dependency: Option<&BlockResult>,
) -> IPtr {
    let object_tracker = object_tracker_tls::get_local_object_tracker_instance();
    let block_result = allocate_and_init!(object_tracker, BlockResult);
    let block_result_data = block_result.read_into_mut::<BlockResult>(None).unwrap();

    block_result_data
        .right_halo
        .resize_to_capacity(algorithm_state.chunk_size);
    block_result_data
        .bottom_halo
        .resize_to_capacity(algorithm_state.chunk_size);

    let mut left = 0;
    let mut current_row = vec![0; algorithm_state.chunk_size + 1];
    let (mut previous_row, mut left_iter) = match mode {
        0 => (vec![0; algorithm_state.chunk_size + 1], None),
        1 => (
            vec![0; algorithm_state.chunk_size + 1],
            Some(right_halo_dependency.unwrap().right_halo.iter()),
        ),
        2 => {
            let mut previous_row = Vec::with_capacity(algorithm_state.chunk_size + 1);
            previous_row.push(0);
            previous_row.extend(bottom_halo_dependency.unwrap().bottom_halo.iter());

            (previous_row, None)
        }
        3 => {
            let element = bottom_right_dependency.unwrap().bottom_right;
            left = element;

            let mut row_data = Vec::with_capacity(algorithm_state.chunk_size + 1);
            row_data.push(element);
            row_data.extend(bottom_halo_dependency.unwrap().bottom_halo.iter());

            (
                row_data,
                Some(right_halo_dependency.unwrap().right_halo.iter()),
            )
        }
        4..=u8::MAX => unreachable!("invalid mode"),
    };

    // FIXME we should be able to directly extract a slice iterator, but this will do for now
    let vertical_string_slice = algorithm_state
        .vertical_string
        .get_slice(vertical_string_start..vertical_string_end);
    let vertical_string_iterator = vertical_string_slice.into_iter();

    for (_idx_i, c_i) in vertical_string_iterator.enumerate() {
        previous_row[0] = left;

        left = match left_iter {
            None => 0,
            Some(ref mut i) => *i.next().expect("insufficient left iterator capacity"),
        };
        current_row[0] = left;

        let horizontal_string_slice = algorithm_state
            .horizontal_string
            .get_slice(horizontal_string_start..horizontal_string_end);
        let horizontal_string_iterator = horizontal_string_slice.into_iter();
        for (idx_j, c_j) in horizontal_string_iterator.enumerate() {
            #[cfg(debug_assertions)]
            println!(
                "Comparing horizontal {} and vertical {}",
                *c_j as char, *c_i as char
            );

            if c_i == c_j {
                current_row[idx_j + 1] = previous_row[idx_j] + algorithm_state.match_score;
            } else {
                current_row[idx_j + 1] = 0;
                current_row[idx_j + 1] = max(
                    current_row[idx_j + 1],
                    previous_row[idx_j] + algorithm_state.mismatch_score,
                );
                current_row[idx_j + 1] = max(
                    current_row[idx_j + 1],
                    current_row[idx_j] + algorithm_state.insertion_score,
                );
                current_row[idx_j + 1] = max(
                    current_row[idx_j + 1],
                    previous_row[idx_j + 1] + algorithm_state.deletion_score,
                );
            }
        }

        block_result_data
            .right_halo
            .push(*current_row.last().unwrap());

        std::mem::swap(&mut current_row, &mut previous_row);
    }

    unsafe {
        block_result_data
            .bottom_halo
            .copy_from_vec(&previous_row[1..].to_vec())
    };
    block_result_data.bottom_right = *previous_row.last().unwrap();

    register_initial!(block_result, object_tracker);
    block_result.iptr_of()
}

#[nandoize_lib]
pub fn set_final_value(algorithm_state: &mut State, final_block_dependency: &BlockResult) {
    algorithm_state.bottom_right_value = final_block_dependency.bottom_right;
}

// FIXME return Result instead
#[nandoize_lib]
pub fn init_smith_waterman(
    // FIXME receive file path instead of string
    horizontal_string: Option<String>,
    horizontal_string_size_kb: Option<usize>,
    // FIXME receive file path instead of string
    vertical_string: Option<String>,
    vertical_string_size_kb: Option<usize>,
    num_chunks_along_dimension: usize,
) -> IPtr {
    let object_tracker = object_tracker_tls::get_local_object_tracker_instance();
    let state = allocate_and_init!(object_tracker, State);
    let state_iptr: IPtr = state.iptr_of();

    let horizontal_string: String = match horizontal_string {
        None => {
            let Some(horizontal_string_size_kb) = horizontal_string_size_kb else {
                eprintln!("missing horizontal string value and size");
                return state_iptr;
            };
            vec!['a'; 1024 * horizontal_string_size_kb].iter().collect()
        }
        Some(horizontal_string) => horizontal_string,
    };

    let vertical_string: String = match vertical_string {
        None => {
            let Some(vertical_string_size_kb) = vertical_string_size_kb else {
                eprintln!("missing vertical string value and size");
                return state_iptr;
            };
            vec!['b'; 1024 * vertical_string_size_kb].iter().collect()
        }
        Some(vertical_string) => vertical_string,
    };

    if horizontal_string.len() != vertical_string.len() {
        let msg = format!(
            "Can only run SW for strings of same length (horizontal string has {} chars, vertical has {})",
            horizontal_string.len(),
            vertical_string.len()
        );
        eprintln!("{msg}");
        return state_iptr;
    }

    let chunk_size =
        (horizontal_string.len() + num_chunks_along_dimension - 1) / num_chunks_along_dimension;

    let state_data = state.read_into_mut::<State>(None).unwrap();

    state_data.chunk_size = chunk_size;
    state_data.match_score = 2;
    state_data.mismatch_score = -1;
    state_data.insertion_score = -1;
    state_data.deletion_score = -1;

    state_data.horizontal_string.from(&horizontal_string);
    state_data.vertical_string.from(&vertical_string);

    let chunks: Vec<(usize, usize)> = (0..num_chunks_along_dimension)
        .map(|i| (i * chunk_size, (i + 1) * chunk_size))
        .collect();
    let mut block_results = vec![
        vec![NandoArgument::get_nil(); num_chunks_along_dimension];
        num_chunks_along_dimension
    ];
    block_results[0][0] = nando_spawn!(
        "smith_waterman::process_block",
        state_iptr,
        0,
        chunks[0].0,
        chunks[0].1,
        chunks[0].0,
        chunks[0].1,
        NandoArgument::get_nil(),
        NandoArgument::get_nil(),
        NandoArgument::get_nil()
    );

    for j in 1..num_chunks_along_dimension {
        block_results[0][j] = nando_yield!(
            "smith_waterman::process_block",
            state_iptr,
            1,
            chunks[j].0,
            chunks[j].1,
            chunks[0].0,
            chunks[0].1,
            block_results[0][j - 1].clone(),
            NandoArgument::get_nil(),
            NandoArgument::get_nil()
        );
    }

    for i in 1..num_chunks_along_dimension {
        block_results[i][0] = nando_yield!(
            "smith_waterman::process_block",
            state_iptr,
            2,
            chunks[0].0,
            chunks[0].1,
            chunks[i].0,
            chunks[i].1,
            NandoArgument::get_nil(),
            block_results[i - 1][0].clone(),
            NandoArgument::get_nil()
        );

        for j in 1..num_chunks_along_dimension {
            block_results[i][j] = nando_yield!(
                "smith_waterman::process_block",
                state_iptr,
                3,
                chunks[j].0,
                chunks[j].1,
                chunks[i].0,
                chunks[i].1,
                block_results[i][j - 1].clone(),
                block_results[i - 1][j].clone(),
                block_results[i - 1][j - 1].clone()
            );
        }
    }

    nando_yield!(
        "smith_waterman::set_final_value",
        state_iptr,
        block_results[num_chunks_along_dimension - 1][num_chunks_along_dimension - 1].clone()
    );

    register_initial!(state, object_tracker);
    state_iptr
}
