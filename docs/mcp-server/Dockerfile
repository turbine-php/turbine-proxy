FROM node:22-alpine

WORKDIR /app
COPY package.json ./
RUN npm install --omit=dev
COPY index.js ./

ENV MCP_PORT=3000
EXPOSE 3000

CMD ["node", "index.js"]
