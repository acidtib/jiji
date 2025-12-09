import { Command } from "@cliffy/command";
import { Confirm } from "@cliffy/prompt";
import { ensureDir } from "@std/fs";

// Read template content from src/jiji.yml
async function getConfigTemplate(): Promise<string> {
  const templatePath = import.meta.dirname + "/../jiji.yml";
  return await Deno.readTextFile(templatePath);
}

export const initCommand = new Command()
  .description("Create config stub in config/jiji.yml")
  .action(async () => {
    console.log("Init command called!");
    console.log("This will create a config stub in config/jiji.yml");

    const configPath = "config/jiji.yml";

    // Check if the config file already exists
    let shouldWrite = true;
    try {
      await Deno.stat(configPath);
      // If we reach this point, the file exists
      // Prompt user if they want to overwrite the existing file
      shouldWrite = await Confirm.prompt({
        message: `Config file already exists at ${configPath}. Overwrite it?`,
        default: false,
      });
    } catch (error) {
      // If file doesn't exist, Deno.stat throws an error, which is expected
      if (error instanceof Deno.errors.NotFound) {
        shouldWrite = true;
      } else {
        console.error("Error checking if config file exists:", error);
        return;
      }
    }

    if (shouldWrite) {
      try {
        // Create the config directory if it doesn't exist
        await ensureDir("config");

        // Get the template content
        const configTemplate = await getConfigTemplate();

        // Write the template content to the file
        await Deno.writeTextFile(configPath, configTemplate);
        console.log(`Config file created successfully at ${configPath}`);
      } catch (error) {
        console.error("Error creating config file:", error);
      }
    } else {
      console.log("Init command cancelled by user.");
    }
  });
