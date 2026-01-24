# Remote Party Finder Reborn

A tool to synchronize FFXIV Party Finder listings to a web interface, integrated with FFLogs for automatic parse data display.

## Disclaimer

This is a fork of the [original Remote Party Finder project](https://github.com/zeroeightysix/remote-party-finder) by zeroeightysix.

I have no plans to publicly release this plugin.

It was created purely out of personal interest and can be considered a proof of concept.

## Key Features

- **Real-time Synchronization**: View in-game Party Finder listings on the web with minimal latency.
- **FFLogs Integration**: automatically fetches and displays Best Performance Average (Parse) for party members and leaders.
  - Supports Batch GraphQL queries for efficient data retrieval (solving N+1 query problems).
  - Caches parse data in MongoDB (24-hour expiration) to respect API rate limits.
- **Modern Web UI**: Clean, responsive interface with parse FFLOGS style color coding.
- **Crowd Sourcing**: Displays data by receiving information from each player using the plugin.

## Architecture

The project consists of two main components:

1.  **Client (Plugin)**: A C# Dalamud Plugin that collects Party Finder data from the game client and sends it to the server.
2.  **Server**: A Rust backend that receives data, communicates with FFLogs API, stores data in MongoDB, and serves the web interface.

### Project Structure

```
/
├── csharp/                 # Dalamud Plugin (C#)
│   ├── RemotePartyFinder/  # Main Plugin Logic
│   └── ...
├── server/                 # Backend Server (Rust)
│   ├── src/
│   │   ├── domain/         # Business Logic (Listing, Player, Stats)
│   │   ├── infra/          # Infrastructure (MongoDB, FFLogs API)
│   │   └── web/            # Web Handlers & Routes
│   ├── templates/          # HTML Templates (Askama)
│   └── assets/             # CSS/JS Assets
└── ...
```

## Setup & Usage

### Prerequisites

- **Rust** (Latest Stable)
- **MongoDB**
- **FFXIV with Dalamud** (for the plugin)
- **FFLogs V2 API Client**

### Server Setup

1.  Navigate to the `server/` directory.
2.  Copy `config.example.toml` to `config.toml`.
3.  Edit `config.toml` and fill in your details:
    ```toml
    [mongo]
    url = "YOUR_MONGODB_CONNECTION_STRING"
    
    [fflogs]
    client_id = "YOUR_CLIENT_ID"
    client_secret = "YOUR_CLIENT_SECRET"
    ```
4.  Run the server:
    ```bash
    cargo run --release
    ```
    The server typically listens on `http://127.0.0.1:8000`.

### Plugin Setup

1.  Open `csharp/RemotePartyFinder.sln` in Visual Studio.
2.  Build the solution.
3.  Load the built plugin in FFXIV using Dalamud's dev plugins feature.
4.  The plugin will automatically start sending Party Finder data to the configured server endpoint.

## License

No license specified yet.
Since the original repository also does not have a license set, the license configuration is postponed.
