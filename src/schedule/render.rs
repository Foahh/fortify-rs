use super::{Spec, Timing};
use crate::{Error, Result, paths};

pub fn checked_text(value: &str) -> Result<&str> {
    if value.chars().any(char::is_control) {
        return Err(Error::Config(
            "scheduler paths cannot contain control characters".into(),
        ));
    }

    Ok(value)
}

pub fn systemd_arg(value: &str) -> Result<String> {
    checked_text(value)?;

    Ok(format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    ))
}

pub fn systemd(spec: &Spec) -> Result<(String, String)> {
    let args = std::iter::once(paths::utf8(&spec.runner)?.to_owned())
        .chain(spec.args()?)
        .map(|a| systemd_arg(&a))
        .collect::<Result<Vec<_>>>()?
        .join(" ");

    let service = format!(
        "# Managed by Fortify.\n\
         [Unit]\n\
         Description=Fortify file organization\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={args}\n"
    );

    let timing = match spec.timing {
        Timing::Every { seconds } => {
            format!(
                "OnActiveSec={seconds}s\n\
                 OnUnitActiveSec={seconds}s"
            )
        }
        Timing::Daily { hour, minute } => {
            format!(
                "OnCalendar=*-*-* {hour:02}:{minute:02}:00\n\
                 Persistent=true"
            )
        }
    };

    let timer = format!(
        "# Managed by Fortify.\n\
         [Unit]\n\
         Description=Fortify schedule\n\
         \n\
         [Timer]\n\
         {timing}\n\
         Unit=fortify.service\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n"
    );

    Ok((service, timer))
}

pub fn xml(value: &str) -> Result<String> {
    checked_text(value)?;

    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}

pub fn launchd(spec: &Spec, label: &str) -> Result<String> {
    let args = std::iter::once(paths::utf8(&spec.runner)?.to_owned())
        .chain(spec.args()?)
        .map(|s| xml(&s).map(|s| format!("<string>{s}</string>")))
        .collect::<Result<Vec<_>>>()?
        .join("\n");

    let timing = match spec.timing {
        Timing::Every { seconds } => {
            format!("<key>StartInterval</key><integer>{seconds}</integer>")
        }
        Timing::Daily { hour, minute } => format!(
            "<key>StartCalendarInterval</key>\
             <dict>\
                 <key>Hour</key><integer>{hour}</integer>\
                 <key>Minute</key><integer>{minute}</integer>\
             </dict>"
        ),
    };

    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\n\
             <key>Label</key><string>{}</string>\n\
             <key>ProgramArguments</key><array>{args}</array>\n\
             <key>ProcessType</key><string>Background</string>\n\
             {timing}\n\
             <key>StandardErrorPath</key><string>{}</string>\n\
         </dict></plist>\n",
        xml(label)?,
        xml(paths::utf8(&spec.log)?)?
    ))
}

// Windows command-line quoting follows CommandLineToArgvW/Rust argument parsing.
pub fn windows_arg(value: &str) -> Result<String> {
    checked_text(value)?;

    let mut result = String::from("\"");
    let mut slashes = 0;

    for c in value.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }

        if c == '"' {
            result.push_str(&"\\".repeat(slashes * 2 + 1));
        } else {
            result.push_str(&"\\".repeat(slashes));
        }

        slashes = 0;
        result.push(c);
    }

    result.push_str(&"\\".repeat(slashes * 2));
    result.push('"');

    Ok(result)
}

pub fn windows_xml(spec: &Spec, user: &str, boundary: &str) -> Result<String> {
    let trigger = match spec.timing {
        Timing::Every { seconds } => format!(
            "<TimeTrigger>\
                 <Repetition>\
                     <Interval>PT{seconds}S</Interval>\
                     <StopAtDurationEnd>false</StopAtDurationEnd>\
                 </Repetition>\
                 <StartBoundary>{}</StartBoundary>\
                 <Enabled>true</Enabled>\
             </TimeTrigger>",
            xml(boundary)?
        ),
        Timing::Daily { .. } => format!(
            "<CalendarTrigger>\
                 <StartBoundary>{}</StartBoundary>\
                 <Enabled>true</Enabled>\
                 <ScheduleByDay>\
                     <DaysInterval>1</DaysInterval>\
                 </ScheduleByDay>\
             </CalendarTrigger>",
            xml(boundary)?
        ),
    };

    let runner = paths::utf8(&spec.runner)?;
    if runner.contains('%') {
        return Err(Error::Config(
            "Windows Task Scheduler expands percent variables in executable paths; \
             install the binaries in a path without '%' before scheduling"
                .into(),
        ));
    }

    let encoded = super::RunnerRequest::from_spec(spec).encode()?;
    let args = format!("--request {encoded}");

    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n\
         <Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n\
             <RegistrationInfo>\
                 <Description>Fortify file organization</Description>\
             </RegistrationInfo>\n\
             <Triggers>{trigger}</Triggers>\n\
             <Principals>\
                 <Principal id=\"User\">\
                     <UserId>{user}</UserId>\
                     <LogonType>InteractiveToken</LogonType>\
                     <RunLevel>LeastPrivilege</RunLevel>\
                 </Principal>\
             </Principals>\n\
             <Settings>\
                 <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\
                 <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\
                 <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\
                 <StartWhenAvailable>true</StartWhenAvailable>\
                 <Enabled>true</Enabled>\
                 <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\
             </Settings>\n\
             <Actions Context=\"User\">\
                 <Exec>\
                     <Command>{runner}</Command>\
                     <Arguments>{args}</Arguments>\
                 </Exec>\
             </Actions>\n\
         </Task>",
        user = xml(user)?,
        runner = xml(paths::utf8(&spec.runner)?)?,
        args = xml(&args)?
    ))
}
