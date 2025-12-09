import { Command } from "@cliffy/command";
import {
  checkEngineAvailability,
  getEngineCommand,
  loadConfig,
} from "../../utils/config.ts";

export const bootstrapCommand = new Command()
  .description("Bootstrap servers with curl and Podman or Docker")
  .option("-c, --config <path:string>", "Path to jiji.yml config file")
  .action(async (options) => {
    try {
      console.log("🖥️  Server bootstrap command called!");
      console.log("Loading configuration...");

      // Load and parse the configuration
      const { config, configPath } = await loadConfig(options.config);
      console.log(`✅ Configuration loaded from: ${configPath}`);
      console.log(`🐳 Container engine: ${config.engine}`);

      // Check if the specified engine is available
      const engineCommand = getEngineCommand(config);
      const isEngineAvailable = await checkEngineAvailability(engineCommand);

      if (!isEngineAvailable) {
        console.error(
          `❌ Container engine '${engineCommand}' is not available or not installed`,
        );
        console.error(
          `Please install ${engineCommand} and make sure it's in your PATH`,
        );
        Deno.exit(1);
      }

      console.log(`✅ Container engine '${engineCommand}' is available`);

      // Display services that will be bootstrapped
      const serviceNames = Object.keys(config.services);
      console.log(
        `📦 Found ${serviceNames.length} service(s): ${
          serviceNames.join(", ")
        }`,
      );

      // Display detailed service information
      console.log("\n📋 Service Details:");
      for (
        const [serviceName, serviceConfig] of Object.entries(config.services)
      ) {
        console.log(`\n  🔹 ${serviceName}:`);

        if (serviceConfig.image) {
          console.log(`    📦 Image: ${serviceConfig.image}`);
        }

        if (serviceConfig.build) {
          console.log(
            `    🔨 Build: ${
              typeof serviceConfig.build === "string"
                ? serviceConfig.build
                : serviceConfig.build.context
            }`,
          );
        }

        if (serviceConfig.ports && serviceConfig.ports.length > 0) {
          console.log(`    🌐 Ports: ${serviceConfig.ports.join(", ")}`);
        }

        if (serviceConfig.volumes && serviceConfig.volumes.length > 0) {
          console.log(`    💾 Volumes: ${serviceConfig.volumes.join(", ")}`);
        }

        if (serviceConfig.environment) {
          const envCount = Array.isArray(serviceConfig.environment)
            ? serviceConfig.environment.length
            : Object.keys(serviceConfig.environment).length;
          console.log(`    🔧 Environment vars: ${envCount} defined`);
        }

        if (serviceConfig.depends_on && serviceConfig.depends_on.length > 0) {
          console.log(
            `    🔗 Depends on: ${serviceConfig.depends_on.join(", ")}`,
          );
        }
      }

      console.log("\n🚧 Bootstrap implementation in progress...");
      console.log("Next steps will include:");
      console.log("- Setting up container networks");
      console.log("- Pulling/building container images");
      console.log("- Creating and starting containers");
      console.log("- Configuring port mappings and volumes");
    } catch (error) {
      console.error("❌ Bootstrap failed:");
      console.error(error instanceof Error ? error.message : String(error));
      Deno.exit(1);
    }
  });
