# Security

`sonic-trace` reads database dumps and prints a report. It makes no network
connections and writes nothing to the dumps it reads.

To report a security problem, use GitHub's private vulnerability reporting
(the **Security** tab of the repository, then **Report a vulnerability**).
Please do not open a public issue for it. This is a personal project, so response
times are best effort.

Inputs are untrusted: a crash, hang or unbounded memory use on a crafted dump is
a bug worth reporting.
