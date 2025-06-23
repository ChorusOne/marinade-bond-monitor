FROM rust:1.84.1 as builder

WORKDIR /app

COPY . .
RUN cargo build --release

# Use a lightweight Node.js image for the final image
FROM node:18-slim

WORKDIR /app

COPY --from=builder /app/target/release/marinade-bond-monitor /usr/local/bin/marinade-bond-monitor

# Install validator-bonds-cli-institutional
RUN npm install -g @marinade.finance/validator-bonds-cli-institutional@latest

# Set the default command
CMD ["marinade-bond-monitor"]
