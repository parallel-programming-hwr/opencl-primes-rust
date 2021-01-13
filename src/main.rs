/*
 * opencl demos with rust
 * Copyright (C) 2021 trivernis
 * See LICENSE for more information
 */

use crate::kernel_controller::primes::is_prime;
use crate::kernel_controller::KernelController;
use crate::output::csv::ThreadedCSVWriter;
use crate::output::threaded::ThreadedWriter;

use crate::kernel_controller::bench::BenchStatistics;
use crate::utils::logging::init_logger;
use ocl_stream::stream::OCLStream;
use ocl_stream::utils::result::{OCLStreamError, OCLStreamResult};
use rayon::prelude::*;
use std::fs::{File, OpenOptions};
use std::io::BufWriter;
use std::path::PathBuf;
use std::time::Duration;
use structopt::StructOpt;

mod benching;
mod kernel_controller;
mod output;
mod utils;

#[derive(StructOpt, Clone, Debug)]
#[structopt()]
enum Opts {
    /// Calculates primes on the GPU
    #[structopt(name = "calculate-primes")]
    CalculatePrimes(CalculatePrimes),

    /// Benchmarks the local size value
    #[structopt(name = "bench-local-size")]
    BenchLocalSize(BenchLocalSize),

    /// Benchmarks the global size (number of tasks) value
    #[structopt(name = "bench-global-size")]
    BenchGlobalSize(BenchGlobalSize),

    /// Prints GPU information
    Info,
}

#[derive(StructOpt, Clone, Debug)]
struct CalculatePrimes {
    /// The number to start with
    #[structopt(long = "start", default_value = "0")]
    start_offset: u64,

    /// The maximum number to calculate to
    #[structopt(long = "end", default_value = "9223372036854775807")]
    max_number: u64,

    /// The output file for the calculated prime numbers
    #[structopt(short = "o", long = "output", default_value = "primes.txt")]
    output_file: PathBuf,

    /// The output file for timings
    #[structopt(long = "timings-output", default_value = "timings.csv")]
    timings_file: PathBuf,

    /// The local size for the tasks.
    /// The value for numbers_per_step needs to be divisible by this number.
    /// The maximum local size depends on the gpu capabilities.
    /// If no value is provided, OpenCL chooses it automatically.
    #[structopt(long = "local-size")]
    local_size: Option<usize>,

    /// The amount of numbers that are checked per step. Even numbers are ignored so the
    /// Range actually goes to numbers_per_step * 2.
    #[structopt(long = "numbers-per-step", default_value = "33554432")]
    numbers_per_step: usize,

    /// If the prime numbers should be used for the divisibility check instead of using
    /// an optimized auto-increment loop.
    #[structopt(long = "no-cache")]
    no_cache: bool,

    /// If the calculated prime numbers should be validated on the cpu by a simple prime algorithm
    #[structopt(long = "cpu-validate")]
    cpu_validate: bool,

    /// number of used threads
    #[structopt(short = "p", long = "parallel", default_value = "2")]
    num_threads: usize,
}

#[derive(StructOpt, Clone, Debug)]
struct BenchLocalSize {
    #[structopt(flatten)]
    bench_options: BenchOptions,

    /// The initial number for the local size
    #[structopt(long = "local-size-start", default_value = "4")]
    local_size_start: usize,

    /// The amount the local size increases by every step
    #[structopt(long = "local-size-step", default_value = "4")]
    local_size_step: usize,

    /// The maximum amount of the local size
    /// Can't be greater than the maximum local size of the gpu
    /// that can be retrieved with the info command
    #[structopt(long = "local-size-stop", default_value = "1024")]
    local_size_stop: usize,

    /// The maximum number of tasks for the benchmark
    #[structopt(long = "global-size", default_value = "6144")]
    global_size: usize,
}

#[derive(StructOpt, Clone, Debug)]
pub struct BenchGlobalSize {
    #[structopt(flatten)]
    options: BenchOptions,

    /// The start value for the used global size
    #[structopt(long = "global-size-start", default_value = "1024")]
    global_size_start: usize,

    /// The step value for the used global size
    #[structopt(long = "global-size-step", default_value = "128")]
    global_size_step: usize,

    /// The stop value for the used global size
    #[structopt(long = "global-size-stop", default_value = "1048576")]
    global_size_stop: usize,

    /// The maximum number of tasks for the benchmark
    #[structopt(long = "local-size", default_value = "128")]
    local_size: usize,
}

#[derive(StructOpt, Clone, Debug)]
pub struct BenchOptions {
    /// How many calculations steps should be done per GPU thread
    #[structopt(short = "n", long = "calculation-steps", default_value = "1000000")]
    calculation_steps: u32,

    /// The output file for timings
    #[structopt(short = "o", long = "bench-output", default_value = "bench.csv")]
    benchmark_file: PathBuf,

    /// The average of n runs that is used instead of using one value only.
    /// By default the benchmark for each step is only run once
    #[structopt(short = "r", long = "repetitions", default_value = "1")]
    repetitions: usize,
}

fn main() -> OCLStreamResult<()> {
    let opts: Opts = Opts::from_args();
    let controller = KernelController::new()?;
    init_logger();

    match opts {
        Opts::Info => controller.print_info().map_err(OCLStreamError::from),
        Opts::CalculatePrimes(prime_opts) => calculate_primes(prime_opts, controller),
        Opts::BenchGlobalSize(bench_opts) => bench_global_size(bench_opts, controller),
        Opts::BenchLocalSize(bench_opts) => bench_local_size(bench_opts, controller),
    }
}

/// Calculates primes on the GPU
fn calculate_primes(
    prime_opts: CalculatePrimes,
    mut controller: KernelController,
) -> OCLStreamResult<()> {
    controller.set_concurrency(prime_opts.num_threads);

    let csv_file = open_write_buffered(&prime_opts.timings_file);
    let mut csv_writer = ThreadedCSVWriter::new(csv_file, &["first", "count", "gpu_duration"]);
    let output_file = open_write_buffered(&prime_opts.output_file);

    let output_writer = ThreadedWriter::new(output_file, |v: Vec<u64>| {
        v.iter()
            .map(|v| v.to_string())
            .fold("".to_string(), |a, b| format!("{}\n{}", a, b))
            .into_bytes()
    });

    let mut stream = controller.calculate_primes(
        prime_opts.start_offset,
        prime_opts.max_number,
        prime_opts.numbers_per_step,
        prime_opts.local_size.unwrap_or(128),
        !prime_opts.no_cache,
    );
    while let Ok(r) = stream.next() {
        let primes = r.value();
        if prime_opts.cpu_validate {
            validate_primes_on_cpu(primes);
        }
        let first = *primes.first().unwrap(); // if there's none, rip
        log::debug!(
            "Calculated {} primes in {:?}, offset: {}",
            primes.len(),
            r.gpu_duration(),
            first
        );
        csv_writer.add_row(vec![
            first.to_string(),
            primes.len().to_string(),
            duration_to_ms_string(r.gpu_duration()),
        ]);
        output_writer.write(primes.clone());
    }
    csv_writer.close();
    output_writer.close();

    Ok(())
}

/// Benchmarks the local size used for calculations
fn bench_local_size(opts: BenchLocalSize, controller: KernelController) -> OCLStreamResult<()> {
    let bench_writer = open_write_buffered(&opts.bench_options.benchmark_file);
    let csv_writer = ThreadedCSVWriter::new(
        bench_writer,
        &[
            "local_size",
            "global_size",
            "calc_count",
            "write_duration",
            "gpu_duration",
            "read_duration",
        ],
    );
    let stream = controller.bench_local_size(
        opts.global_size,
        opts.local_size_start,
        opts.local_size_step,
        opts.local_size_stop,
        opts.bench_options.calculation_steps,
        opts.bench_options.repetitions,
    )?;
    read_bench_results(opts.bench_options.calculation_steps, csv_writer, stream);

    Ok(())
}

/// Benchmarks the global size used for calculations
fn bench_global_size(opts: BenchGlobalSize, controller: KernelController) -> OCLStreamResult<()> {
    let bench_writer = open_write_buffered(&opts.options.benchmark_file);
    let csv_writer = ThreadedCSVWriter::new(
        bench_writer,
        &[
            "local_size",
            "global_size",
            "calc_count",
            "write_duration",
            "gpu_duration",
            "read_duration",
        ],
    );
    let stream = controller.bench_global_size(
        opts.local_size,
        opts.global_size_start,
        opts.global_size_step,
        opts.global_size_stop,
        opts.options.calculation_steps,
        opts.options.repetitions,
    )?;
    read_bench_results(opts.options.calculation_steps, csv_writer, stream);

    Ok(())
}

/// Reads benchmark results from the stream and prints
/// them to the console
fn read_bench_results(
    calculation_steps: u32,
    mut csv_writer: ThreadedCSVWriter,
    mut stream: OCLStream<BenchStatistics>,
) {
    loop {
        match stream.next() {
            Ok(stats) => {
                log::debug!("{:?}", stats);
                csv_writer.add_row(vec![
                    stats.local_size.to_string(),
                    stats.global_size.to_string(),
                    calculation_steps.to_string(),
                    duration_to_ms_string(&stats.write_duration),
                    duration_to_ms_string(&stats.calc_duration),
                    duration_to_ms_string(&stats.read_duration),
                ])
            }
            _ => {
                break;
            }
        }
    }
    csv_writer.close();
}

fn validate_primes_on_cpu(primes: &Vec<u64>) {
    log::debug!("Validating primes on the cpu");
    let failures = primes
        .par_iter()
        .filter(|n| !is_prime(**n))
        .collect::<Vec<&u64>>();
    if failures.len() > 0 {
        panic!(
            "{} failures in prime calculation: {:?}",
            failures.len(),
            failures
        );
    } else {
        log::debug!("No failures found.");
    }
}

fn duration_to_ms_string(duration: &Duration) -> String {
    format!("{}", duration.as_secs_f64() * 1000f64)
}

/// opens a file in a buffered writer
/// if it already exists it will be recreated
fn open_write_buffered(path: &PathBuf) -> BufWriter<File> {
    BufWriter::new(
        OpenOptions::new()
            .truncate(true)
            .write(true)
            .create(true)
            .open(path)
            .expect("Failed to open file!"),
    )
}
