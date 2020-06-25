# openweathermap-exporter

Exposes current weather data for a particular location as a set of Prometheus
metrics.

## Usage

 1. Request a free API key from https://openweathermap.org/api
 2. Run:
    ```bash
    openweathermap-exporter $LAT,$LON --api-key "$KEY" --units imperial
    ```
 3. View the results:
    ```bash
    curl http://localhost:8081
    ```

If there's an error fetching the current report from OpenWeatherMap, the
exporter will export only `owm_error 1` (possibly with an HTTP status code).
Refer to the exporter logs for details.
