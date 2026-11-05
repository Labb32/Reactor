use crate::plan::PlanVersionRecord;
use std::{sync::mpsc, thread, time::Duration};

pub(super) fn finish_capture(
    stdout: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    stderr: thread::JoinHandle<std::io::Result<Vec<u8>>>,
) -> std::io::Result<(Vec<u8>, Vec<u8>)> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let output = join_capture(stdout);
        let error = join_capture(stderr);
        let _ = sender.send(output.and_then(|output| error.map(|error| (output, error))));
    });
    receiver
        .recv_timeout(Duration::from_secs(60))
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "output collection timed out")
        })?
}

fn join_capture(handle: thread::JoinHandle<std::io::Result<Vec<u8>>>) -> std::io::Result<Vec<u8>> {
    handle
        .join()
        .map_err(|_| std::io::Error::other("log capture thread panicked"))?
}

pub(super) fn join_input(handle: thread::JoinHandle<std::io::Result<()>>) -> std::io::Result<()> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(
            handle
                .join()
                .map_err(|_| std::io::Error::other("agent input thread panicked"))
                .and_then(|result| result),
        );
    });
    receiver.recv_timeout(Duration::from_secs(2)).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::TimedOut, "agent input close timed out")
    })?
}

pub(super) fn execution_prompt(plan: &PlanVersionRecord, task_index: u32) -> String {
    let mut prompt = format!(
        "Execute the reviewed task below. Work autonomously within the configured sandbox. Do not request interactive approval.\n\n<task>\n{}\n</task>",
        plan.context
    );
    if let Some(task) = plan.tasks.get(task_index as usize) {
        prompt.push_str(&format!("\n\n<current_task id=\"{}\">\n{}\n</current_task>\nExecute only this Task. Files from previous Tasks are already present. In your final response, optionally report warnings as JSON: {{\"reactor_task_result\":{{\"warnings\":[{{\"level\":\"info\",\"message\":\"description\"}}]}}}}. Use level blocking when operator review is required. Do not claim business correctness on behalf of the scheduler.", task.id, task.context));
    }
    if !plan.completion_criteria.is_empty() {
        prompt.push_str("\n\n<completion_criteria>\n");
        for (index, criterion) in plan.completion_criteria.iter().enumerate() {
            prompt.push_str(&format!("{}. {}\n", index + 1, criterion));
        }
        prompt.push_str("</completion_criteria>");
    }
    prompt
}
